//! Player statistics: typed definitions (fold kind, visibility, constraints, cycles, community
//! aggregate, client-writable, AVGRATE window), per-player values per scope, cycle-scoped values
//! (daily/weekly/monthly/custom), game-wide aggregates and idempotent updates.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/stats/defs", get(defs).put(put_defs))
        .route("/v1/server/stats/update", post(update))
        .route("/v1/server/stats/top", get(top))
        .route("/v1/server/stats/global", get(server_global))
        .route("/v1/server/stats/{account_id}", get(server_read))
        .route("/v1/server/stats/{account_id}/history", get(server_history))
        .route("/v1/stats", get(mine).post(client_update))
        .route("/v1/stats/global", get(global))
        .route("/v1/stats/history", get(my_history))
        .route("/v1/stats/{account_id}", get(theirs))
}

const KINDS: [&str; 6] = ["sum", "max", "min", "last", "avg", "avgrate"];
const VISIBILITY: [&str; 3] = ["public", "friends", "private"];
const CYCLE_KINDS: [&str; 4] = ["daily", "weekly", "monthly", "custom"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cycle {
    pub kind: String,
    /// `custom` length.
    pub period_seconds: i64,
    /// Hour of day (UTC) at which the cycle rolls over.
    pub reset_hour: i32,
}

impl Default for Cycle {
    fn default() -> Self {
        Self {
            kind: "daily".into(),
            period_seconds: 0,
            reset_hour: 0,
        }
    }
}

impl Cycle {
    fn validate(&self) -> Result<()> {
        if !CYCLE_KINDS.contains(&self.kind.as_str()) {
            return Err(Error::InvalidArgument(
                "cycle kind: daily|weekly|monthly|custom".into(),
            ));
        }
        if self.kind == "custom" && self.period_seconds < 60 {
            return Err(Error::InvalidArgument(
                "custom cycle: period_seconds >= 60".into(),
            ));
        }
        if !(0..24).contains(&self.reset_hour) {
            return Err(Error::InvalidArgument("cycle reset_hour: 0..23".into()));
        }
        Ok(())
    }

    /// Start of the cycle containing `now`.
    pub fn start(&self, now: OffsetDateTime) -> OffsetDateTime {
        let shift = self.reset_hour as i64 * 3600;
        match self.kind.as_str() {
            "monthly" => {
                let d = now.date().replace_day(1).unwrap_or(now.date());
                let mut start = d.midnight().assume_utc() + Duration::seconds(shift);
                if start > now {
                    let (y, m) = match d.month().previous() {
                        time::Month::December => (d.year() - 1, time::Month::December),
                        m => (d.year(), m),
                    };
                    start = time::Date::from_calendar_date(y, m, 1)
                        .unwrap_or(d)
                        .midnight()
                        .assume_utc()
                        + Duration::seconds(shift);
                }
                start
            }
            kind => {
                let p = match kind {
                    "daily" => 86_400,
                    "weekly" => 604_800,
                    _ => self.period_seconds.max(60),
                };
                let t = now.unix_timestamp() - shift;
                OffsetDateTime::from_unix_timestamp(t.div_euclid(p) * p + shift)
                    .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            }
        }
    }

    /// Start of the next cycle after the one containing `now`.
    pub fn next_start(&self, now: OffsetDateTime) -> OffsetDateTime {
        let s = self.start(now);
        match self.kind.as_str() {
            "monthly" => self.start(s + Duration::days(32)),
            "daily" => s + Duration::days(1),
            "weekly" => s + Duration::weeks(1),
            _ => s + Duration::seconds(self.period_seconds.max(60)),
        }
    }

    /// Scope key under which cycle values are stored.
    pub fn scope(&self, now: OffsetDateTime) -> String {
        format!("@{}:{}", self.kind, self.start(now).unix_timestamp())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Def {
    pub id: String,
    pub name: String,
    /// sum | max | min | last | avg | avgrate
    pub kind: String,
    /// public | friends | private
    pub visibility: String,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    /// Largest absolute change one update may make.
    pub max_change: Option<f64>,
    /// Updates may never lower the value.
    pub increment_only: bool,
    /// Starting value before the first update (sum/avgrate start here).
    pub default_value: f64,
    /// Cycle-scoped copies of the value (daily/weekly/monthly/custom).
    pub cycles: Vec<Cycle>,
    /// Also fold every update into a game-wide aggregate.
    pub global: bool,
    /// Players may write this stat themselves (`POST /v1/stats`).
    pub client_writable: bool,
    /// AVGRATE window in seconds (0 = plain cumulative rate).
    pub window_secs: i64,
    pub metadata: Value,
}

impl Default for Def {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: "sum".into(),
            visibility: "public".into(),
            min_value: None,
            max_value: None,
            max_change: None,
            increment_only: false,
            default_value: 0.0,
            cycles: Vec::new(),
            global: false,
            client_writable: false,
            window_secs: 0,
            metadata: json!({}),
        }
    }
}

impl Def {
    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return Err(Error::InvalidArgument(format!("stat id {:?}", self.id)));
        }
        if !KINDS.contains(&self.kind.as_str()) {
            return Err(Error::InvalidArgument(
                "kind must be sum|max|min|last|avg|avgrate".into(),
            ));
        }
        if !VISIBILITY.contains(&self.visibility.as_str()) {
            return Err(Error::InvalidArgument(
                "visibility must be public|friends|private".into(),
            ));
        }
        if self.name.chars().count() > 128 {
            return Err(Error::InvalidArgument("name longer than 128 chars".into()));
        }
        if let (Some(a), Some(b)) = (self.min_value, self.max_value) {
            if a > b {
                return Err(Error::InvalidArgument("min_value > max_value".into()));
            }
        }
        if self.max_change.is_some_and(|m| m <= 0.0 || m.is_nan()) {
            return Err(Error::InvalidArgument("max_change must be positive".into()));
        }
        if !self.default_value.is_finite() || self.window_secs < 0 {
            return Err(Error::InvalidArgument(
                "default_value finite, window_secs non-negative".into(),
            ));
        }
        if self.cycles.len() > 8 {
            return Err(Error::InvalidArgument("at most 8 cycles".into()));
        }
        let mut kinds = std::collections::HashSet::new();
        for c in &self.cycles {
            c.validate()?;
            if !kinds.insert(c.kind.as_str()) {
                return Err(Error::InvalidArgument(format!(
                    "duplicate cycle {}",
                    c.kind
                )));
            }
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

type DefRow = (
    String,
    String,
    String,
    String,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    bool,
    f64,
    Value,
    bool,
    bool,
    i64,
    Value,
);
const DEF_COLS: &str = "id, name, kind, visibility, min_value, max_value, max_change, increment_only, default_value, cycles, global, client_writable, window_secs, metadata";

fn def(r: DefRow) -> Def {
    Def {
        id: r.0,
        name: r.1,
        kind: r.2,
        visibility: r.3,
        min_value: r.4,
        max_value: r.5,
        max_change: r.6,
        increment_only: r.7,
        default_value: r.8,
        cycles: serde_json::from_value(r.9).unwrap_or_default(),
        global: r.10,
        client_writable: r.11,
        window_secs: r.12,
        metadata: r.13,
    }
}

async fn defs(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "defs": load_defs(&s.db).await? })))
}

pub async fn load_defs(db: &PgPool) -> Result<Vec<Def>> {
    let rows: Vec<DefRow> = sqlx::query_as(&format!(
        "select {DEF_COLS} from musubi.stat_defs order by id"
    ))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows.into_iter().map(def).collect())
}

async fn def_map(db: &PgPool) -> Result<BTreeMap<String, Def>> {
    Ok(load_defs(db)
        .await?
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect())
}

#[derive(Deserialize)]
struct PutDefs {
    defs: Vec<Def>,
    #[serde(default)]
    replace: bool,
}

async fn put_defs(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<PutDefs>,
) -> Result<Json<Value>, ApiError> {
    for d in &r.defs {
        d.validate()?;
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for d in &r.defs {
        sqlx::query(
            "insert into musubi.stat_defs (id, name, kind, visibility, min_value, max_value, max_change, increment_only,
                                           default_value, cycles, global, client_writable, window_secs, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             on conflict (id) do update set name = excluded.name, kind = excluded.kind,
                 visibility = excluded.visibility, min_value = excluded.min_value, max_value = excluded.max_value,
                 max_change = excluded.max_change, increment_only = excluded.increment_only,
                 default_value = excluded.default_value, cycles = excluded.cycles, global = excluded.global,
                 client_writable = excluded.client_writable, window_secs = excluded.window_secs,
                 metadata = excluded.metadata, updated_at = now()",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(&d.kind)
        .bind(&d.visibility)
        .bind(d.min_value)
        .bind(d.max_value)
        .bind(d.max_change)
        .bind(d.increment_only)
        .bind(d.default_value)
        .bind(serde_json::to_value(&d.cycles).map_err(Error::internal)?)
        .bind(d.global)
        .bind(d.client_writable)
        .bind(d.window_secs)
        .bind(&d.metadata)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    if r.replace {
        let keep: Vec<&str> = r.defs.iter().map(|d| d.id.as_str()).collect();
        sqlx::query("delete from musubi.stat_defs where id <> all($1)")
            .bind(&keep)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(Json(json!({ "defs": load_defs(&s.db).await? })))
}

/// One update: a number, or `{value, length}` (AVGRATE: `value` over `length` seconds).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum StatIn {
    Num(f64),
    Rate { value: f64, length: f64 },
}

impl StatIn {
    fn value(&self) -> f64 {
        match self {
            StatIn::Num(v) | StatIn::Rate { value: v, .. } => *v,
        }
    }
    fn length(&self) -> Option<f64> {
        match self {
            StatIn::Num(_) => None,
            StatIn::Rate { length, .. } => Some(*length),
        }
    }
}

/// Stored state of one (stat, scope) row.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cell {
    pub value: f64,
    pub samples: i64,
    pub num: f64,
    pub den: f64,
}

/// Fold one update into a cell under the definition's kind and constraints.
pub fn fold(d: &Def, old: Option<Cell>, input: StatIn) -> Result<Cell> {
    let v = input.value();
    if !v.is_finite() {
        return Err(Error::InvalidArgument(format!(
            "stat {}: not a finite number",
            d.id
        )));
    }
    let first = old.is_none();
    let o = old.unwrap_or(Cell {
        value: d.default_value,
        ..Cell::default()
    });
    let mut c = Cell {
        samples: o.samples + 1,
        ..o
    };
    c.value = match d.kind.as_str() {
        "max" if first => v,
        "min" if first => v,
        "max" => o.value.max(v),
        "min" => o.value.min(v),
        "last" => v,
        "avg" => (o.value * o.samples as f64 + v) / (o.samples as f64 + 1.0),
        "avgrate" => {
            let len = input.length().unwrap_or(1.0);
            if len <= 0.0 || !len.is_finite() {
                return Err(Error::InvalidArgument(format!(
                    "stat {}: avgrate needs a positive length",
                    d.id
                )));
            }
            let decay = if d.window_secs > 0 {
                (1.0 - len / d.window_secs as f64).max(0.0)
            } else {
                1.0
            };
            c.num = o.num * decay + v;
            c.den = o.den * decay + len;
            c.num / c.den
        }
        _ => o.value + v,
    };
    if d.increment_only && !first && c.value < o.value {
        return Err(Error::InvalidArgument(format!(
            "stat {} is increment-only",
            d.id
        )));
    }
    if let Some(m) = d.max_change {
        if (c.value - o.value).abs() > m + 1e-9 {
            return Err(Error::InvalidArgument(format!(
                "stat {}: change {} exceeds max_change {m}",
                d.id,
                c.value - o.value
            )));
        }
    }
    if let Some(lo) = d.min_value {
        c.value = c.value.max(lo);
    }
    if let Some(hi) = d.max_value {
        c.value = c.value.min(hi);
    }
    Ok(c)
}

async fn fold_row(
    tx: &mut Transaction<'_, Postgres>,
    d: &Def,
    account: Option<AccountId>,
    scope: &str,
    input: StatIn,
) -> Result<Cell> {
    let old: Option<(f64, i64, f64, f64)> = match account {
        Some(a) => {
            sqlx::query_as(
                "select value, samples, num, den from musubi.player_stats where account_id = $1 and stat_id = $2 and scope = $3 for update",
            )
            .bind(a.0)
            .bind(&d.id)
            .bind(scope)
            .fetch_optional(&mut **tx)
            .await
        }
        None => {
            sqlx::query_as(
                "select value, samples, num, den from musubi.global_stats where stat_id = $1 and scope = $2 for update",
            )
            .bind(&d.id)
            .bind(scope)
            .fetch_optional(&mut **tx)
            .await
        }
    }
    .map_err(Error::internal)?;
    let c = fold(
        d,
        old.map(|(value, samples, num, den)| Cell {
            value,
            samples,
            num,
            den,
        }),
        input,
    )?;
    match account {
        Some(a) => {
            sqlx::query(
                "insert into musubi.player_stats (account_id, stat_id, scope, value, samples, num, den)
                 values ($1, $2, $3, $4, $5, $6, $7)
                 on conflict (account_id, stat_id, scope) do update set value = excluded.value,
                     samples = excluded.samples, num = excluded.num, den = excluded.den, updated_at = now()",
            )
            .bind(a.0)
            .bind(&d.id)
            .bind(scope)
            .bind(c.value)
            .bind(c.samples)
            .bind(c.num)
            .bind(c.den)
            .execute(&mut **tx)
            .await
        }
        None => {
            sqlx::query(
                "insert into musubi.global_stats (stat_id, scope, value, samples, num, den)
                 values ($1, $2, $3, $4, $5, $6)
                 on conflict (stat_id, scope) do update set value = excluded.value,
                     samples = excluded.samples, num = excluded.num, den = excluded.den, updated_at = now()",
            )
            .bind(&d.id)
            .bind(scope)
            .bind(c.value)
            .bind(c.samples)
            .bind(c.num)
            .bind(c.den)
            .execute(&mut **tx)
            .await
        }
    }
    .map_err(Error::internal)?;
    Ok(c)
}

fn cell_json(c: Cell) -> Value {
    json!({ "value": c.value, "samples": c.samples })
}

/// Apply a batch of updates for one player: the given scope, the all-time slice, every cycle of
/// each stat and (for `global` stats) the game-wide aggregate. `transaction_id` makes the whole
/// batch idempotent.
pub async fn apply_full(
    db: &PgPool,
    account: AccountId,
    scope: &str,
    stats: &BTreeMap<String, StatIn>,
    by_client: bool,
    transaction_id: Option<&str>,
) -> Result<Value> {
    if scope.len() > 64 || scope.starts_with('@') {
        return Err(Error::InvalidArgument(
            "scope: <= 64 chars, '@' is reserved for cycles".into(),
        ));
    }
    let defs = def_map(db).await?;
    let now = OffsetDateTime::now_utc();
    let mut applied = serde_json::Map::new();
    let mut unknown = Vec::new();
    let mut tx = db.begin().await.map_err(Error::internal)?;
    if let Some(tid) = transaction_id {
        if tid.is_empty() || tid.len() > 128 {
            return Err(Error::InvalidArgument(
                "transaction_id: 1..=128 chars".into(),
            ));
        }
        let n = sqlx::query(
            "insert into musubi.stat_transactions (id) values ($1) on conflict do nothing",
        )
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Ok(json!({ "applied": {}, "unknown": [], "duplicate": true }));
        }
    }
    for (id, input) in stats {
        let Some(d) = defs.get(id) else {
            unknown.push(id.clone());
            continue;
        };
        if by_client && !d.client_writable {
            return Err(Error::PermissionDenied(format!(
                "stat {id} is not client-writable"
            )));
        }
        let mut out = json!({});
        let mut scopes: Vec<(String, &str)> = vec![(scope.to_string(), "scope")];
        if !scope.is_empty() {
            scopes.push((String::new(), "all"));
        } else {
            scopes[0].1 = "all";
        }
        for (sc, key) in &scopes {
            let c = fold_row(&mut tx, d, Some(account), sc, *input).await?;
            out[*key] = cell_json(c);
            if d.global {
                let g = fold_row(&mut tx, d, None, sc, *input).await?;
                out["global"][*key] = cell_json(g);
            }
        }
        for cy in &d.cycles {
            let sc = cy.scope(now);
            let c = fold_row(&mut tx, d, Some(account), &sc, *input).await?;
            out["cycles"][&cy.kind] = cell_json(c);
            if d.global {
                let g = fold_row(&mut tx, d, None, &sc, *input).await?;
                out["global"]["cycles"][&cy.kind] = cell_json(g);
            }
        }
        applied.insert(id.clone(), out);
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(json!({ "applied": applied, "unknown": unknown, "duplicate": false }))
}

/// Plain numeric batch (match results and other internal callers).
pub async fn apply(
    db: &PgPool,
    account: AccountId,
    scope: &str,
    stats: &BTreeMap<String, f64>,
) -> Result<Value> {
    let m = stats
        .iter()
        .map(|(k, v)| (k.clone(), StatIn::Num(*v)))
        .collect();
    apply_full(db, account, scope, &m, false, None).await
}

#[derive(Deserialize)]
struct UpdateReq {
    account_id: Uuid,
    #[serde(default)]
    scope: String,
    stats: BTreeMap<String, StatIn>,
    transaction_id: Option<String>,
}

async fn update(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<UpdateReq>,
) -> Result<Json<Value>, ApiError> {
    if r.stats.is_empty() || r.stats.len() > 64 {
        return Err(Error::InvalidArgument("1..=64 stats per update".into()).into());
    }
    Ok(Json(
        apply_full(
            &s.db,
            AccountId(r.account_id),
            &r.scope,
            &r.stats,
            false,
            r.transaction_id.as_deref(),
        )
        .await?,
    ))
}

#[derive(Deserialize)]
struct ClientUpdateReq {
    #[serde(default)]
    scope: String,
    stats: BTreeMap<String, StatIn>,
    transaction_id: Option<String>,
}

/// Players write their own `client_writable` stats (casual / offline games).
async fn client_update(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<ClientUpdateReq>,
) -> Result<Json<Value>, ApiError> {
    if r.stats.is_empty() || r.stats.len() > 64 {
        return Err(Error::InvalidArgument("1..=64 stats per update".into()).into());
    }
    let tid = r
        .transaction_id
        .as_deref()
        .map(|t| format!("{}:{t}", me.account_id));
    Ok(Json(
        apply_full(
            &s.db,
            me.account_id,
            &r.scope,
            &r.stats,
            true,
            tid.as_deref(),
        )
        .await?,
    ))
}

#[derive(Deserialize, Default)]
struct ReadQuery {
    scope: Option<String>,
}

fn cycle_json(cy: &Cycle, c: Option<Cell>, now: OffsetDateTime) -> Value {
    let mut v = c
        .map(cell_json)
        .unwrap_or(json!({ "value": Value::Null, "samples": 0 }));
    v["period_start"] = json!(cy.start(now).unix_timestamp());
    v["resets_at"] = json!(cy.next_start(now).unix_timestamp());
    v
}

async fn read(
    db: &PgPool,
    account: AccountId,
    scope: Option<&str>,
    visible: &[&str],
) -> Result<Value> {
    let defs = def_map(db).await?;
    let now = OffsetDateTime::now_utc();
    let rows: Vec<(String, String, f64, i64)> = sqlx::query_as(
        "select p.stat_id, p.scope, p.value, p.samples
         from musubi.player_stats p join musubi.stat_defs d on d.id = p.stat_id
         where p.account_id = $1 and d.visibility = any($2)
         order by p.stat_id, p.scope",
    )
    .bind(account.0)
    .bind(visible)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let mut stats = serde_json::Map::new();
    for (id, sc, value, samples) in rows {
        let Some(d) = defs.get(&id) else { continue };
        let e = stats.entry(id.clone()).or_insert_with(|| {
            let mut cycles = serde_json::Map::new();
            for cy in &d.cycles {
                cycles.insert(cy.kind.clone(), cycle_json(cy, None, now));
            }
            json!({ "kind": d.kind, "visibility": d.visibility, "scopes": {}, "cycles": cycles, "metadata": d.metadata })
        });
        let cell = Cell {
            value,
            samples,
            ..Cell::default()
        };
        if let Some(rest) = sc.strip_prefix('@') {
            let Some(cy) = d.cycles.iter().find(|c| sc == c.scope(now)) else {
                continue;
            };
            let _ = rest;
            e["cycles"][&cy.kind] = cycle_json(cy, Some(cell), now);
        } else if scope.is_none_or(|s| s == sc) {
            e["scopes"][sc.as_str()] = cell_json(cell);
        }
    }
    Ok(json!({ "account_id": account, "stats": stats }))
}

async fn server_read(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(account_id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read(
            &s.db,
            AccountId(account_id),
            q.scope.as_deref(),
            &VISIBILITY,
        )
        .await?,
    ))
}

async fn mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read(&s.db, me.account_id, q.scope.as_deref(), &VISIBILITY).await?,
    ))
}

async fn theirs(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(account_id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    let target = AccountId(account_id);
    if target == me.account_id {
        return Ok(Json(
            read(&s.db, target, q.scope.as_deref(), &VISIBILITY).await?,
        ));
    }
    if s.social.is_blocked(target, me.account_id).await? {
        return Err(Error::NotFound("account".into()).into());
    }
    let friend: Option<i16> = sqlx::query_scalar(
        "select state from musubi.friends where user_id = $1 and friend_id = $2 and state = 0",
    )
    .bind(target.0)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let visible: &[&str] = if friend.is_some() {
        &["public", "friends"]
    } else {
        &["public"]
    };
    Ok(Json(
        read(&s.db, target, q.scope.as_deref(), visible).await?,
    ))
}

#[derive(Deserialize)]
struct HistoryQuery {
    stat: String,
    #[serde(default = "daily")]
    cycle: String,
    #[serde(default = "twelve")]
    limit: i64,
}

fn daily() -> String {
    "daily".into()
}
fn twelve() -> i64 {
    12
}

/// Past cycle values of one stat, newest first.
async fn history(
    db: &PgPool,
    account: AccountId,
    q: &HistoryQuery,
    visible: &[&str],
) -> Result<Value> {
    let defs = def_map(db).await?;
    let Some(d) = defs.get(&q.stat) else {
        return Err(Error::NotFound(format!("stat {}", q.stat)));
    };
    if !visible.contains(&d.visibility.as_str()) {
        return Err(Error::NotFound(format!("stat {}", q.stat)));
    }
    let Some(cy) = d.cycles.iter().find(|c| c.kind == q.cycle) else {
        return Err(Error::InvalidArgument(format!(
            "stat {} has no {} cycle",
            q.stat, q.cycle
        )));
    };
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        "select scope, value, samples from musubi.player_stats
         where account_id = $1 and stat_id = $2 and scope like $3 order by scope desc limit $4",
    )
    .bind(account.0)
    .bind(&q.stat)
    .bind(format!("@{}:%", q.cycle))
    .bind(q.limit.clamp(1, 366))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    let current = cy.scope(now);
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(sc, value, samples)| {
            let start: i64 = sc.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(0);
            json!({ "period_start": start, "value": value, "samples": samples, "current": sc == current })
        })
        .collect();
    Ok(json!({
        "stat": q.stat, "cycle": q.cycle, "kind": d.kind, "entries": entries,
        "current_period_start": cy.start(now).unix_timestamp(),
    }))
}

async fn my_history(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(history(&s.db, me.account_id, &q, &VISIBILITY).await?))
}

async fn server_history(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(account_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        history(&s.db, AccountId(account_id), &q, &VISIBILITY).await?,
    ))
}

/// Game-wide aggregates of `global` stats.
async fn read_global(db: &PgPool, scope: Option<&str>, visible: &[&str]) -> Result<Value> {
    let defs = def_map(db).await?;
    let now = OffsetDateTime::now_utc();
    let rows: Vec<(String, String, f64, i64)> = sqlx::query_as(
        "select g.stat_id, g.scope, g.value, g.samples from musubi.global_stats g
         join musubi.stat_defs d on d.id = g.stat_id where d.visibility = any($1) order by g.stat_id, g.scope",
    )
    .bind(visible)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let mut stats = serde_json::Map::new();
    for d in defs
        .values()
        .filter(|d| d.global && visible.contains(&d.visibility.as_str()))
    {
        let mut cycles = serde_json::Map::new();
        for cy in &d.cycles {
            cycles.insert(cy.kind.clone(), cycle_json(cy, None, now));
        }
        stats.insert(
            d.id.clone(),
            json!({ "kind": d.kind, "scopes": {}, "cycles": cycles, "metadata": d.metadata }),
        );
    }
    for (id, sc, value, samples) in rows {
        let (Some(d), Some(e)) = (defs.get(&id), stats.get_mut(&id)) else {
            continue;
        };
        let cell = Cell {
            value,
            samples,
            ..Cell::default()
        };
        if sc.starts_with('@') {
            if let Some(cy) = d.cycles.iter().find(|c| sc == c.scope(now)) {
                e["cycles"][&cy.kind] = cycle_json(cy, Some(cell), now);
            }
        } else if scope.is_none_or(|s| s == sc) {
            e["scopes"][sc.as_str()] = cell_json(cell);
        }
    }
    Ok(json!({ "stats": stats }))
}

async fn global(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read_global(&s.db, q.scope.as_deref(), &["public", "friends"]).await?,
    ))
}

async fn server_global(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read_global(&s.db, q.scope.as_deref(), &VISIBILITY).await?,
    ))
}

#[derive(Deserialize)]
struct TopQuery {
    stat: String,
    #[serde(default)]
    scope: String,
    /// Rank within the current cycle of this kind instead of the scope.
    cycle: Option<String>,
    #[serde(default = "fifty")]
    limit: i64,
}

fn fifty() -> i64 {
    50
}

async fn top(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<TopQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.clamp(1, 500);
    let defs = def_map(&s.db).await?;
    let Some(d) = defs.get(&q.stat) else {
        return Err(Error::NotFound(format!("stat {}", q.stat)).into());
    };
    let scope = match &q.cycle {
        Some(kind) => {
            let Some(cy) = d.cycles.iter().find(|c| &c.kind == kind) else {
                return Err(
                    Error::InvalidArgument(format!("stat {} has no {kind} cycle", q.stat)).into(),
                );
            };
            cy.scope(OffsetDateTime::now_utc())
        }
        None => q.scope.clone(),
    };
    let order = if d.kind == "min" { "asc" } else { "desc" };
    let rows: Vec<(Uuid, f64, i64)> = sqlx::query_as(&format!(
        "select account_id, value, samples from musubi.player_stats
         where stat_id = $1 and scope = $2 order by value {order}, account_id limit $3"
    ))
    .bind(&q.stat)
    .bind(&scope)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let entries: Vec<Value> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (a, v, n))| json!({ "rank": i + 1, "account_id": a, "value": v, "samples": n }))
        .collect();
    Ok(Json(
        json!({ "stat": q.stat, "scope": scope, "kind": d.kind, "entries": entries }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(kind: &str) -> Def {
        Def {
            id: "s".into(),
            kind: kind.into(),
            ..Default::default()
        }
    }

    #[test]
    fn folds_by_kind() {
        let f =
            |k: &str, old: Option<Cell>, v: f64| fold(&d(k), old, StatIn::Num(v)).unwrap().value;
        let c = Some(Cell {
            value: 10.0,
            samples: 2,
            ..Cell::default()
        });
        assert_eq!(f("sum", c, 5.0), 15.0);
        assert_eq!(f("max", c, 5.0), 10.0);
        assert_eq!(f("min", c, 5.0), 5.0);
        assert_eq!(f("last", c, 5.0), 5.0);
        assert_eq!(f("avg", c, 4.0), 8.0);
        assert_eq!(
            f("max", None, -3.0),
            -3.0,
            "first max is the sample, not the default"
        );
        let mut dd = d("sum");
        dd.default_value = 100.0;
        assert_eq!(fold(&dd, None, StatIn::Num(1.0)).unwrap().value, 101.0);
    }

    #[test]
    fn avgrate_windows_and_constraints() {
        let mut r = d("avgrate");
        r.window_secs = 100;
        let c1 = fold(
            &r,
            None,
            StatIn::Rate {
                value: 10.0,
                length: 50.0,
            },
        )
        .unwrap();
        assert_eq!(c1.value, 0.2);
        // second session of 50 s: the first one is weighted by (1 - 50/100) = 0.5
        let c2 = fold(
            &r,
            Some(c1),
            StatIn::Rate {
                value: 0.0,
                length: 50.0,
            },
        )
        .unwrap();
        assert!((c2.value - 5.0 / 75.0).abs() < 1e-9, "{}", c2.value);
        assert!(fold(
            &r,
            None,
            StatIn::Rate {
                value: 1.0,
                length: 0.0
            }
        )
        .is_err());
        let mut inc = d("last");
        inc.increment_only = true;
        let c = Some(Cell {
            value: 5.0,
            samples: 1,
            ..Cell::default()
        });
        assert!(fold(&inc, c, StatIn::Num(4.0)).is_err());
        assert_eq!(fold(&inc, c, StatIn::Num(6.0)).unwrap().value, 6.0);
        let mut lim = d("sum");
        lim.max_change = Some(10.0);
        lim.max_value = Some(12.0);
        assert!(fold(&lim, c, StatIn::Num(11.0)).is_err());
        assert_eq!(
            fold(&lim, c, StatIn::Num(9.0)).unwrap().value,
            12.0,
            "clamped to max_value"
        );
    }

    #[test]
    fn cycles_align_and_roll() {
        let now = OffsetDateTime::from_unix_timestamp(10 * 86_400 + 3 * 3600).unwrap();
        let daily = Cycle {
            kind: "daily".into(),
            reset_hour: 5,
            ..Default::default()
        };
        assert_eq!(daily.start(now).unix_timestamp(), 9 * 86_400 + 5 * 3600);
        assert_eq!(
            daily.next_start(now).unix_timestamp(),
            10 * 86_400 + 5 * 3600
        );
        assert_eq!(
            daily.scope(now),
            format!("@daily:{}", 9 * 86_400 + 5 * 3600)
        );
        let monthly = Cycle {
            kind: "monthly".into(),
            ..Default::default()
        };
        let mar = time::Date::from_calendar_date(2026, time::Month::March, 15)
            .unwrap()
            .midnight()
            .assume_utc();
        assert_eq!(
            monthly.start(mar).date(),
            time::Date::from_calendar_date(2026, time::Month::March, 1).unwrap()
        );
        assert_eq!(
            monthly.next_start(mar).date(),
            time::Date::from_calendar_date(2026, time::Month::April, 1).unwrap()
        );
        let jan1 = time::Date::from_calendar_date(2026, time::Month::January, 1)
            .unwrap()
            .midnight()
            .assume_utc();
        let m5 = Cycle {
            kind: "monthly".into(),
            reset_hour: 5,
            ..Default::default()
        };
        assert_eq!(
            m5.start(jan1).date(),
            time::Date::from_calendar_date(2025, time::Month::December, 1).unwrap(),
            "before the reset hour → previous month"
        );
        assert!(Cycle {
            kind: "custom".into(),
            period_seconds: 10,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
