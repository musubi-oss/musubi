//! Hero classes and heroes (LootLocker classes & heroes parity): an operator defines classes —
//! loadout slots plus Can Equip / Can't Equip rules by item category or item id — and a player
//! creates heroes of those classes (`max_per_player` each, one default), each with its own
//! loadout. An inventory entry is equipped on one hero at a time; equipping it elsewhere moves
//! it. Other players' heroes and loadouts follow the owner's `inventory` privacy level.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/hero_classes", get(classes))
        .route(
            "/v1/server/hero_classes/{id}",
            get(class_get).put(class_put).delete(class_delete),
        )
        .route("/v1/server/accounts/{id}/heroes", get(server_heroes))
        .route("/v1/hero_classes", get(client_classes))
        .route("/v1/heroes", get(my_heroes).post(create_hero))
        .route(
            "/v1/heroes/{id}",
            get(hero_get).put(hero_put).delete(hero_delete),
        )
        .route("/v1/heroes/{id}/loadout", get(my_loadout))
        .route("/v1/heroes/{id}/equip", post(equip))
        .route("/v1/heroes/{id}/unequip", post(unequip))
        .route("/v1/users/{uid}/heroes", get(their_heroes))
        .route("/v1/users/{uid}/heroes/{id}/loadout", get(their_loadout))
}

fn rfc(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn code_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct EquipRule {
    categories: Vec<String>,
    items: Vec<String>,
}

impl EquipRule {
    fn matches(&self, category: &str, item: &str) -> bool {
        self.categories.iter().any(|c| c == category) || self.items.iter().any(|i| i == item)
    }
    fn is_empty(&self) -> bool {
        self.categories.is_empty() && self.items.is_empty()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClassBody {
    name: String,
    description: String,
    slots: Vec<String>,
    can_equip: Value,
    cant_equip: Value,
    max_per_player: Option<i32>,
    metadata: Value,
    enabled: Option<bool>,
}

type ClassRow = (
    String,
    String,
    String,
    Vec<String>,
    Value,
    Value,
    i32,
    Value,
    bool,
    OffsetDateTime,
);
const CLASS_COLS: &str =
    "id, name, description, slots, can_equip, cant_equip, max_per_player, metadata, enabled, updated_at";

fn class_json(r: &ClassRow) -> Value {
    json!({
        "id": r.0, "name": r.1, "description": r.2, "slots": r.3, "can_equip": r.4,
        "cant_equip": r.5, "max_per_player": r.6, "metadata": r.7, "enabled": r.8,
        "updated_at": rfc(r.9),
    })
}

fn parse_rule(v: &Value, what: &str) -> Result<EquipRule> {
    if v.is_null() {
        return Ok(EquipRule::default());
    }
    let r: EquipRule = serde_json::from_value(v.clone())
        .map_err(|e| Error::InvalidArgument(format!("{what}: {e}")))?;
    if r.categories.len() + r.items.len() > 200 {
        return Err(Error::InvalidArgument(format!(
            "{what}: at most 200 entries"
        )));
    }
    Ok(r)
}

async fn class_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<ClassBody>,
) -> Result<Json<Value>, ApiError> {
    if !code_ok(&id, 64) {
        return Err(Error::InvalidArgument("class id: 1..=64 of [A-Za-z0-9_-]".into()).into());
    }
    if b.name.trim().is_empty() || b.name.len() > 64 {
        return Err(Error::InvalidArgument("name: 1..=64 chars".into()).into());
    }
    if b.description.len() > 1000 {
        return Err(Error::InvalidArgument("description: at most 1000 chars".into()).into());
    }
    if b.slots.len() > 32 || b.slots.iter().any(|sl| !code_ok(sl, 64)) {
        return Err(
            Error::InvalidArgument("slots: at most 32 codes of [A-Za-z0-9_-]".into()).into(),
        );
    }
    let max = b.max_per_player.unwrap_or(10);
    if !(1..=100).contains(&max) {
        return Err(Error::InvalidArgument("max_per_player: 1..=100".into()).into());
    }
    parse_rule(&b.can_equip, "can_equip")?;
    parse_rule(&b.cant_equip, "cant_equip")?;
    if !(b.metadata.is_null() || b.metadata.is_object()) {
        return Err(Error::InvalidArgument("metadata: object".into()).into());
    }
    let row: ClassRow = sqlx::query_as(&format!(
        "insert into musubi.hero_classes (id, name, description, slots, can_equip, cant_equip, max_per_player, metadata, enabled)
         values ($1, $2, $3, $4, coalesce($5, '{{}}'::jsonb), coalesce($6, '{{}}'::jsonb), $7, coalesce($8, '{{}}'::jsonb), $9)
         on conflict (id) do update set name = excluded.name, description = excluded.description,
             slots = excluded.slots, can_equip = excluded.can_equip, cant_equip = excluded.cant_equip,
             max_per_player = excluded.max_per_player, metadata = excluded.metadata,
             enabled = excluded.enabled, updated_at = now()
         returning {CLASS_COLS}"
    ))
    .bind(&id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(&b.slots)
    .bind((!b.can_equip.is_null()).then_some(&b.can_equip))
    .bind((!b.cant_equip.is_null()).then_some(&b.cant_equip))
    .bind(max)
    .bind((!b.metadata.is_null()).then_some(&b.metadata))
    .bind(b.enabled.unwrap_or(true))
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit("hero_class.updated", None, json!({"class_id": id}))
        .await;
    Ok(Json(class_json(&row)))
}

async fn load_class(s: &AppState, id: &str) -> Result<ClassRow> {
    sqlx::query_as(&format!(
        "select {CLASS_COLS} from musubi.hero_classes where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("hero class {id}")))
}

async fn class_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load_class(&s, &id).await?;
    let (heroes,): (i64,) =
        sqlx::query_as("select count(*) from musubi.heroes where class_id = $1")
            .bind(&id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    let mut v = class_json(&r);
    v["heroes"] = json!(heroes);
    Ok(Json(v))
}

async fn list_classes(s: &AppState, only_enabled: bool) -> Result<Value> {
    let rows: Vec<ClassRow> = sqlx::query_as(&format!(
        "select {CLASS_COLS} from musubi.hero_classes where (not $1 or enabled) order by id"
    ))
    .bind(only_enabled)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({ "classes": rows.iter().map(class_json).collect::<Vec<_>>() }))
}

async fn classes(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(list_classes(&s, false).await?))
}

async fn client_classes(
    State(s): State<AppState>,
    Bearer(_): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(list_classes(&s, true).await?))
}

async fn class_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.hero_classes where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("hero class {id}")).into());
    }
    s.events
        .emit("hero_class.deleted", None, json!({"class_id": id}))
        .await;
    Ok(Json(json!({"deleted": true})))
}

// ---- heroes ----

type HeroRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    bool,
    Value,
    OffsetDateTime,
    OffsetDateTime,
    i64,
);
const HERO_SQL: &str = "select h.id, h.account_id, h.class_id, c.name, h.name, h.is_default, h.metadata, h.created_at, h.updated_at,
    (select count(*) from musubi.hero_loadout l where l.hero_id = h.id)
    from musubi.heroes h join musubi.hero_classes c on c.id = h.class_id";

fn hero_json(r: &HeroRow) -> Value {
    json!({
        "id": r.0, "account_id": r.1, "class_id": r.2, "class_name": r.3, "name": r.4,
        "default": r.5, "metadata": r.6, "created_at": rfc(r.7), "updated_at": rfc(r.8),
        "equipped": r.9,
    })
}

async fn heroes_of(s: &AppState, account: Uuid) -> Result<Vec<HeroRow>> {
    sqlx::query_as(&format!(
        "{HERO_SQL} where h.account_id = $1 order by h.is_default desc, h.created_at"
    ))
    .bind(account)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)
}

/// `id` is a hero uuid or `default` (the account's default hero).
async fn resolve(s: &AppState, account: Uuid, id: &str) -> Result<HeroRow> {
    let row: Option<HeroRow> = if id == "default" {
        sqlx::query_as(&format!(
            "{HERO_SQL} where h.account_id = $1 and h.is_default"
        ))
        .bind(account)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
    } else {
        let hid: Uuid = id
            .parse()
            .map_err(|_| Error::InvalidArgument("hero id: uuid or `default`".into()))?;
        sqlx::query_as(&format!("{HERO_SQL} where h.account_id = $1 and h.id = $2"))
            .bind(account)
            .bind(hid)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?
    };
    row.ok_or_else(|| Error::NotFound("hero".into()))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HeroBody {
    class_id: String,
    name: String,
    metadata: Value,
    default: Option<bool>,
}

fn check_hero_body(name: &str, metadata: &Value) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 32 {
        return Err(Error::InvalidArgument("name: 1..=32 chars".into()));
    }
    if !(metadata.is_null() || metadata.is_object())
        || serde_json::to_vec(metadata).map(|v| v.len()).unwrap_or(0) > 4096
    {
        return Err(Error::InvalidArgument("metadata: object ≤ 4 KB".into()));
    }
    Ok(())
}

async fn create_hero(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<HeroBody>,
) -> Result<Json<Value>, ApiError> {
    check_hero_body(&b.name, &b.metadata)?;
    let class = load_class(&s, &b.class_id).await?;
    if !class.8 {
        return Err(Error::NotFound(format!("hero class {}", b.class_id)).into());
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("heroes:{}", me.account_id.0))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    let (of_class, total): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where class_id = $2), count(*) from musubi.heroes where account_id = $1",
    )
    .bind(me.account_id.0)
    .bind(&b.class_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if of_class >= class.6 as i64 {
        return Err(Error::Conflict(format!(
            "at most {} heroes of class {}",
            class.6, b.class_id
        ))
        .into());
    }
    let make_default = total == 0 || b.default.unwrap_or(false);
    if make_default {
        sqlx::query(
            "update musubi.heroes set is_default = false where account_id = $1 and is_default",
        )
        .bind(me.account_id.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.heroes (id, account_id, class_id, name, is_default, metadata)
         values ($1, $2, $3, $4, $5, coalesce($6, '{}'::jsonb))",
    )
    .bind(id)
    .bind(me.account_id.0)
    .bind(&b.class_id)
    .bind(b.name.trim())
    .bind(make_default)
    .bind((!b.metadata.is_null()).then_some(&b.metadata))
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    let row = resolve(&s, me.account_id.0, &id.to_string()).await?;
    s.events
        .emit(
            "hero.created",
            Some(me.account_id),
            json!({"hero_id": id, "class_id": b.class_id}),
        )
        .await;
    Ok(Json(hero_json(&row)))
}

async fn my_heroes(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows = heroes_of(&s, me.account_id.0).await?;
    Ok(Json(
        json!({ "heroes": rows.iter().map(hero_json).collect::<Vec<_>>() }),
    ))
}

async fn hero_get(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = resolve(&s, me.account_id.0, &id).await?;
    let mut v = hero_json(&row);
    v["loadout"] = loadout_json(&s, row.0).await?;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HeroPatch {
    name: Option<String>,
    metadata: Option<Value>,
    default: Option<bool>,
}

async fn hero_put(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<HeroPatch>,
) -> Result<Json<Value>, ApiError> {
    let row = resolve(&s, me.account_id.0, &id).await?;
    let name = b.name.clone().unwrap_or_else(|| row.4.clone());
    let metadata = b.metadata.clone().unwrap_or_else(|| row.6.clone());
    check_hero_body(&name, &metadata)?;
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    if b.default == Some(true) {
        sqlx::query(
            "update musubi.heroes set is_default = false where account_id = $1 and is_default",
        )
        .bind(me.account_id.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    if b.default == Some(false) && row.5 {
        return Err(Error::InvalidArgument(
            "pick another default hero instead of unsetting this one".into(),
        )
        .into());
    }
    sqlx::query(
        "update musubi.heroes set name = $2, metadata = $3, is_default = is_default or $4, updated_at = now() where id = $1",
    )
    .bind(row.0)
    .bind(name.trim())
    .bind(&metadata)
    .bind(b.default == Some(true))
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    let row = resolve(&s, me.account_id.0, &row.0.to_string()).await?;
    s.events
        .emit(
            "hero.updated",
            Some(me.account_id),
            json!({"hero_id": row.0}),
        )
        .await;
    Ok(Json(hero_json(&row)))
}

async fn hero_delete(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = resolve(&s, me.account_id.0, &id).await?;
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query("delete from musubi.heroes where id = $1")
        .bind(row.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    if row.5 {
        // the oldest remaining hero becomes the default
        sqlx::query(
            "update musubi.heroes set is_default = true where id = (
                 select id from musubi.heroes where account_id = $1 order by created_at limit 1)",
        )
        .bind(me.account_id.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    s.events
        .emit(
            "hero.deleted",
            Some(me.account_id),
            json!({"hero_id": row.0}),
        )
        .await;
    Ok(Json(json!({"deleted": true})))
}

// ---- loadout ----

type LoadoutRow = (
    String,
    Uuid,
    String,
    String,
    String,
    i64,
    Value,
    OffsetDateTime,
);

async fn loadout_json(s: &AppState, hero: Uuid) -> Result<Value> {
    let rows: Vec<LoadoutRow> = sqlx::query_as(
        "select l.slot, l.inventory_id, i.item_id, d.name, d.category, i.quantity, i.instance, l.equipped_at
         from musubi.hero_loadout l
         join musubi.inventory i on i.id = l.inventory_id
         join musubi.items d on d.id = i.item_id
         where l.hero_id = $1 order by l.slot",
    )
    .bind(hero)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "slot": r.0, "inventory_id": r.1, "item_id": r.2, "name": r.3,
                    "category": r.4, "quantity": r.5, "instance": r.6, "equipped_at": rfc(r.7),
                })
            })
            .collect(),
    ))
}

async fn my_loadout(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = resolve(&s, me.account_id.0, &id).await?;
    Ok(Json(
        json!({ "hero_id": row.0, "class_id": row.2, "loadout": loadout_json(&s, row.0).await? }),
    ))
}

#[derive(Deserialize)]
struct EquipBody {
    inventory_id: Uuid,
    #[serde(default)]
    slot: Option<String>,
}

async fn equip(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<EquipBody>,
) -> Result<Json<Value>, ApiError> {
    let hero = resolve(&s, me.account_id.0, &id).await?;
    let class = load_class(&s, &hero.2).await?;
    let can = parse_rule(&class.4, "can_equip")?;
    let cant = parse_rule(&class.5, "cant_equip")?;
    let entry: Option<(String, String, Vec<String>, String, bool)> = sqlx::query_as(
        "select i.item_id, d.category, d.slots, d.bind, i.bound from musubi.inventory i
         join musubi.items d on d.id = i.item_id where i.id = $1 and i.account_id = $2",
    )
    .bind(b.inventory_id)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((item_id, category, slots, bind, bound)) = entry else {
        return Err(Error::NotFound("inventory entry".into()).into());
    };
    if slots.is_empty() {
        return Err(Error::InvalidArgument(format!("{item_id} is not equippable")).into());
    }
    let slot = match b.slot {
        Some(sl) => sl,
        None if slots.len() == 1 => slots[0].clone(),
        None => {
            return Err(Error::InvalidArgument(format!(
                "{item_id} fits several slots {slots:?}: give slot"
            ))
            .into())
        }
    };
    if !slots.contains(&slot) {
        return Err(Error::InvalidArgument(format!(
            "{item_id} does not fit slot {slot} (fits {slots:?})"
        ))
        .into());
    }
    if !class.3.is_empty() && !class.3.contains(&slot) {
        return Err(Error::InvalidArgument(format!(
            "class {} has no slot {slot} (has {:?})",
            class.0, class.3
        ))
        .into());
    }
    if cant.matches(&category, &item_id) {
        return Err(Error::InvalidArgument(format!(
            "class {} cannot equip {item_id} ({category})",
            class.0
        ))
        .into());
    }
    if !can.is_empty() && !can.matches(&category, &item_id) {
        return Err(Error::InvalidArgument(format!(
            "class {} can only equip {:?} / {:?}",
            class.0, can.categories, can.items
        ))
        .into());
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    // one hero at a time: the entry leaves its previous hero, the slot frees its previous entry
    sqlx::query(
        "delete from musubi.hero_loadout where inventory_id = $1 or (hero_id = $2 and slot = $3)",
    )
    .bind(b.inventory_id)
    .bind(hero.0)
    .bind(&slot)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.hero_loadout (hero_id, slot, inventory_id) values ($1, $2, $3)",
    )
    .bind(hero.0)
    .bind(&slot)
    .bind(b.inventory_id)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if !bound && bind == "equip" {
        sqlx::query("update musubi.inventory set bound = true, updated_at = now() where id = $1")
            .bind(b.inventory_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    s.events
        .emit(
            "hero.equipped",
            Some(me.account_id),
            json!({"hero_id": hero.0, "slot": slot, "inventory_id": b.inventory_id, "item_id": item_id}),
        )
        .await;
    Ok(Json(
        json!({ "hero_id": hero.0, "loadout": loadout_json(&s, hero.0).await? }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UnequipBody {
    slot: Option<String>,
    inventory_id: Option<Uuid>,
}

async fn unequip(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<UnequipBody>,
) -> Result<Json<Value>, ApiError> {
    let hero = resolve(&s, me.account_id.0, &id).await?;
    if b.slot.is_none() && b.inventory_id.is_none() {
        return Err(Error::InvalidArgument("give slot or inventory_id".into()).into());
    }
    let n = sqlx::query(
        "delete from musubi.hero_loadout where hero_id = $1
           and ($2::text is null or slot = $2) and ($3::uuid is null or inventory_id = $3)",
    )
    .bind(hero.0)
    .bind(&b.slot)
    .bind(b.inventory_id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("nothing equipped there".into()).into());
    }
    s.events
        .emit(
            "hero.unequipped",
            Some(me.account_id),
            json!({"hero_id": hero.0, "slot": b.slot, "inventory_id": b.inventory_id}),
        )
        .await;
    Ok(Json(
        json!({ "hero_id": hero.0, "loadout": loadout_json(&s, hero.0).await? }),
    ))
}

// ---- other players / server ----

async fn visible(s: &AppState, owner: Uuid, viewer: Uuid) -> Result<()> {
    let p = super::privacy::load(s, owner).await?;
    if !super::privacy::allowed(s, owner, Some(viewer), &p.inventory).await? {
        return Err(Error::NotFound("user".into()));
    }
    Ok(())
}

async fn their_heroes(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(uid): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    visible(&s, uid, me.account_id.0).await?;
    let rows = heroes_of(&s, uid).await?;
    Ok(Json(
        json!({ "heroes": rows.iter().map(hero_json).collect::<Vec<_>>() }),
    ))
}

async fn their_loadout(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((uid, id)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    visible(&s, uid, me.account_id.0).await?;
    let row = resolve(&s, uid, &id).await?;
    Ok(Json(
        json!({ "hero_id": row.0, "class_id": row.2, "loadout": loadout_json(&s, row.0).await? }),
    ))
}

async fn server_heroes(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows = heroes_of(&s, id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut v = hero_json(r);
        v["loadout"] = loadout_json(&s, r.0).await?;
        out.push(v);
    }
    Ok(Json(json!({ "heroes": out })))
}
