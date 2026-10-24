//! Game sessions (P1-16/56): one object per running game — a dedicated-server allocation, an
//! authoritative match or custom connect info — carrying searchable `properties`, opaque `data`,
//! a join policy with invite codes, per-player seat reservations the game server confirms, and
//! templates players can instantiate for private/custom matches.

use crate::lobby::validate_attrs;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const KINDS: [&str; 3] = ["dedicated", "match", "custom"];
pub const JOIN_POLICIES: [&str; 3] = ["open", "invite", "closed"];
/// A reserved seat the game server has not confirmed within this long stops counting.
pub const RESERVATION_TTL_SECS: i64 = 120;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_DATA_BYTES: usize = 16 * 1024;

fn gen_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

fn gen_token() -> String {
    format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    )
}

pub fn normalize_code(code: &str) -> String {
    code.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

const PLAYERS: &str = "(select count(*) from musubi.game_session_members m where m.session_id = s.id \
    and (m.state = 'joined' or (m.state = 'reserved' and m.reserved_at > now() - make_interval(secs => 120))))";

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    kind: String,
    state: String,
    owner_id: Option<Uuid>,
    template_id: Option<String>,
    fleet_id: Option<Uuid>,
    server_id: Option<Uuid>,
    match_id: Option<String>,
    region: String,
    connect: Value,
    properties: Value,
    data: Value,
    max_players: i32,
    join_policy: String,
    join_code: Option<String>,
    idempotency_key: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    terminated_at: Option<OffsetDateTime>,
    players: i64,
}

fn cols() -> String {
    format!(
        "s.id, s.name, s.kind, s.state, s.owner_id, s.template_id, s.fleet_id, s.server_id, s.match_id, s.region, \
         s.connect, s.properties, s.data, s.max_players, s.join_policy, s.join_code, s.idempotency_key, \
         s.created_at, s.updated_at, s.terminated_at, {PLAYERS} as players"
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub owner_id: Option<AccountId>,
    pub template_id: Option<String>,
    pub fleet_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub match_id: Option<String>,
    pub region: String,
    /// Address/port/connect token; only members, the owner and servers see it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect: Option<Value>,
    pub properties: Value,
    pub data: Value,
    pub max_players: i32,
    pub players: i64,
    pub free_slots: Option<i64>,
    pub join_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_code: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub terminated_at: Option<i64>,
}

impl Session {
    /// Strip connect info and the join code (for search results and non-members).
    pub fn public(mut self) -> Self {
        self.connect = None;
        self.join_code = None;
        self
    }
}

fn to_session(r: Row) -> Session {
    let free = (r.max_players > 0).then(|| (r.max_players as i64 - r.players).max(0));
    Session {
        id: r.id,
        name: r.name,
        kind: r.kind,
        state: r.state,
        owner_id: r.owner_id.map(AccountId),
        template_id: r.template_id,
        fleet_id: r.fleet_id,
        server_id: r.server_id,
        match_id: r.match_id,
        region: r.region,
        connect: Some(r.connect),
        properties: r.properties,
        data: r.data,
        max_players: r.max_players,
        players: r.players,
        free_slots: free,
        join_policy: r.join_policy,
        join_code: r.join_code,
        idempotency_key: r.idempotency_key,
        created_at: r.created_at.unix_timestamp(),
        updated_at: r.updated_at.unix_timestamp(),
        terminated_at: r.terminated_at.map(|t| t.unix_timestamp()),
    }
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    session_id: Uuid,
    account_id: Uuid,
    state: String,
    token: String,
    reserved_at: OffsetDateTime,
    joined_at: Option<OffsetDateTime>,
    left_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub session_id: Uuid,
    pub account_id: AccountId,
    /// `invited` | `reserved` | `joined` | `left`
    pub state: String,
    /// Seat token the player presents to the game server, which confirms it through
    /// `POST /v1/gameserver/session/accept`. Only shown to the player and to servers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub reserved_at: i64,
    pub joined_at: Option<i64>,
    pub left_at: Option<i64>,
    /// Reserved seat older than the TTL and never confirmed.
    pub expired: bool,
}

fn to_member(r: MemberRow, now: OffsetDateTime) -> Member {
    let expired = r.state == "reserved"
        && r.reserved_at + time::Duration::seconds(RESERVATION_TTL_SECS) < now;
    Member {
        session_id: r.session_id,
        account_id: AccountId(r.account_id),
        state: r.state,
        token: Some(r.token),
        reserved_at: r.reserved_at.unix_timestamp(),
        joined_at: r.joined_at.map(|t| t.unix_timestamp()),
        left_at: r.left_at.map(|t| t.unix_timestamp()),
        expired,
    }
}

const MEMBER_COLS: &str = "session_id, account_id, state, token, reserved_at, joined_at, left_at";

pub struct NewSession {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub owner_id: Option<AccountId>,
    pub template_id: Option<String>,
    pub fleet_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub match_id: Option<String>,
    pub region: String,
    pub connect: Value,
    pub properties: Value,
    pub data: Value,
    pub max_players: i32,
    pub join_policy: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UpdateSession {
    pub name: Option<String>,
    pub properties: Option<Value>,
    pub data: Option<Value>,
    pub max_players: Option<i32>,
    pub join_policy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Filter {
    /// Property key, or built-in `name`, `region`, `kind`, `state`, `owner_id`, `join_policy`,
    /// `player_count`, `free_slots`, `max_players`, `created_at`.
    pub key: String,
    /// `eq` | `ne` | `gt` | `gte` | `lt` | `lte` | `contains` | `in` | `exists`
    pub op: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sort {
    pub key: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub near: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SearchSessions {
    pub filters: Vec<Filter>,
    pub sort: Vec<Sort>,
    pub region: Option<String>,
    pub kind: Option<String>,
    pub has_slots: Option<bool>,
    /// Server search only: include terminated sessions (history).
    pub include_terminated: bool,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPage {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    #[serde(skip_deserializing)]
    pub id: String,
    pub name: String,
    /// Pool or fleet name to allocate a dedicated server from.
    pub fleet: Option<String>,
    pub region: Option<String>,
    /// Authoritative match module to launch instead of a dedicated server.
    pub module: Option<String>,
    /// Allocation payload / match params; the session id, owner and properties are merged in.
    pub params: Value,
    pub max_players: i32,
    pub join_policy: String,
    pub max_active_per_owner: i32,
    /// Cold-start wait when no dedicated server is ready (`0` = fail fast).
    pub wait_secs: i32,
    pub enabled: bool,
}

impl Default for Template {
    fn default() -> Self {
        Template {
            id: String::new(),
            name: String::new(),
            fleet: None,
            region: None,
            module: None,
            params: json!({}),
            max_players: 0,
            join_policy: "invite".into(),
            max_active_per_owner: 1,
            wait_secs: 0,
            enabled: true,
        }
    }
}

impl Template {
    pub fn validate(&self) -> Result<()> {
        if self.fleet.is_some() == self.module.is_some() {
            return Err(Error::InvalidArgument(
                "template needs exactly one of fleet or module".into(),
            ));
        }
        if !(1..=1000).contains(&self.max_players) {
            return Err(Error::InvalidArgument("max_players: 1..=1000".into()));
        }
        if !JOIN_POLICIES.contains(&self.join_policy.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "join_policy: one of {JOIN_POLICIES:?}"
            )));
        }
        if !(1..=100).contains(&self.max_active_per_owner) {
            return Err(Error::InvalidArgument(
                "max_active_per_owner: 1..=100".into(),
            ));
        }
        if !(0..=300).contains(&self.wait_secs) {
            return Err(Error::InvalidArgument("wait_secs: 0..=300".into()));
        }
        if !self.params.is_object() && !self.params.is_null() {
            return Err(Error::InvalidArgument("params must be an object".into()));
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.chars().count() > 64 {
        return Err(Error::InvalidArgument("name: at most 64 chars".into()));
    }
    Ok(())
}

fn validate_data(v: &Value) -> Result<Value> {
    if v.is_null() {
        return Ok(json!({}));
    }
    if v.to_string().len() > MAX_DATA_BYTES {
        return Err(Error::InvalidArgument("data: at most 16 KiB".into()));
    }
    Ok(v.clone())
}

fn validate_policy(p: &str) -> Result<()> {
    if !JOIN_POLICIES.contains(&p) {
        return Err(Error::InvalidArgument(format!(
            "join_policy: one of {JOIN_POLICIES:?}"
        )));
    }
    Ok(())
}

fn validate_max(n: i32) -> Result<()> {
    if !(0..=1000).contains(&n) {
        return Err(Error::InvalidArgument("max_players: 0..=1000".into()));
    }
    Ok(())
}

pub fn parse_id(id: &str) -> Result<Uuid> {
    id.parse()
        .map_err(|_| Error::InvalidArgument("bad session id".into()))
}

enum Bind {
    Num(f64),
    Str(String),
    Strs(Vec<String>),
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

fn key_exprs(key: &str) -> Result<(String, String)> {
    let builtin: Option<(String, bool)> = match key {
        "name" => Some(("s.name".into(), false)),
        "region" => Some(("s.region".into(), false)),
        "kind" => Some(("s.kind".into(), false)),
        "state" => Some(("s.state".into(), false)),
        "join_policy" => Some(("s.join_policy".into(), false)),
        "owner_id" => Some(("s.owner_id::text".into(), false)),
        "player_count" => Some((PLAYERS.into(), true)),
        "free_slots" => Some((format!("(s.max_players - {PLAYERS})"), true)),
        "max_players" => Some(("s.max_players".into(), true)),
        "created_at" => Some(("extract(epoch from s.created_at)".into(), true)),
        _ => None,
    };
    if let Some((col, numeric)) = builtin {
        return Ok(if numeric {
            (format!("({col})::text"), format!("({col})::numeric"))
        } else {
            (
                col.clone(),
                format!("case when {col} ~ '^-?[0-9]+(\\.[0-9]+)?$' then ({col})::numeric end"),
            )
        });
    }
    if !valid_key(key) {
        return Err(Error::InvalidArgument(format!("bad property key {key:?}")));
    }
    Ok((
        format!("(s.properties->>'{key}')"),
        format!("case when jsonb_typeof(s.properties->'{key}') = 'number' then (s.properties->>'{key}')::numeric end"),
    ))
}

fn scalar_text(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Error::InvalidArgument(
            "filter value must be a scalar".into(),
        )),
    }
}

#[derive(Clone)]
pub struct GameSessionService {
    db: PgPool,
}

impl GameSessionService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn find_idempotent(&self, key: &str) -> Result<Option<Session>> {
        let sql = format!(
            "select {} from musubi.game_sessions s where s.idempotency_key = $1",
            cols()
        );
        Ok(sqlx::query_as::<_, Row>(&sql)
            .bind(key)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(to_session))
    }

    pub async fn insert(&self, n: NewSession) -> Result<Session> {
        validate_name(&n.name)?;
        if !KINDS.contains(&n.kind.as_str()) {
            return Err(Error::InvalidArgument(format!("kind: one of {KINDS:?}")));
        }
        validate_policy(&n.join_policy)?;
        validate_max(n.max_players)?;
        let properties = validate_attrs(&n.properties, "properties")?;
        let data = validate_data(&n.data)?;
        let code = (n.join_policy != "open").then(gen_code);
        let sql = "insert into musubi.game_sessions (id, name, kind, owner_id, template_id, fleet_id, server_id, match_id, \
             region, connect, properties, data, max_players, join_policy, join_code, idempotency_key) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)";
        let r = sqlx::query(sql)
            .bind(n.id)
            .bind(&n.name)
            .bind(&n.kind)
            .bind(n.owner_id.map(|a| a.0))
            .bind(&n.template_id)
            .bind(n.fleet_id)
            .bind(n.server_id)
            .bind(&n.match_id)
            .bind(&n.region)
            .bind(&n.connect)
            .bind(&properties)
            .bind(&data)
            .bind(n.max_players)
            .bind(&n.join_policy)
            .bind(&code)
            .bind(&n.idempotency_key)
            .execute(&self.db)
            .await;
        match r {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                if let Some(k) = &n.idempotency_key {
                    if let Some(existing) = self.find_idempotent(k).await? {
                        return Ok(existing);
                    }
                }
                return Err(Error::Conflict("session already exists".into()));
            }
            Err(e) => return Err(Error::internal(e)),
        }
        self.get(n.id).await
    }

    pub async fn get(&self, id: Uuid) -> Result<Session> {
        let sql = format!(
            "select {} from musubi.game_sessions s where s.id = $1",
            cols()
        );
        sqlx::query_as::<_, Row>(&sql)
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(to_session)
            .ok_or_else(|| Error::NotFound("session".into()))
    }

    pub async fn by_server(&self, server_id: Uuid) -> Result<Session> {
        let sql = format!(
            "select {} from musubi.game_sessions s where s.server_id = $1 and s.state <> 'terminated' \
             order by s.created_at desc limit 1",
            cols()
        );
        sqlx::query_as::<_, Row>(&sql)
            .bind(server_id)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(to_session)
            .ok_or_else(|| Error::NotFound("session for this server".into()))
    }

    pub async fn by_code(&self, code: &str) -> Result<Session> {
        let code = normalize_code(code);
        if code.is_empty() {
            return Err(Error::InvalidArgument("code is required".into()));
        }
        let sql = format!(
            "select {} from musubi.game_sessions s where s.join_code = $1 and s.state <> 'terminated'",
            cols()
        );
        sqlx::query_as::<_, Row>(&sql)
            .bind(&code)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(to_session)
            .ok_or_else(|| Error::NotFound("no session with this code".into()))
    }

    pub async fn update(&self, id: Uuid, u: UpdateSession) -> Result<Session> {
        if let Some(n) = &u.name {
            validate_name(n)?;
        }
        if let Some(p) = &u.join_policy {
            validate_policy(p)?;
        }
        if let Some(m) = u.max_players {
            validate_max(m)?;
        }
        let properties = u
            .properties
            .as_ref()
            .map(|p| validate_attrs(p, "properties"))
            .transpose()?;
        let data = u.data.as_ref().map(validate_data).transpose()?;
        let n = sqlx::query(
            "update musubi.game_sessions set name = coalesce($2, name), properties = coalesce($3, properties), \
             data = coalesce($4, data), max_players = coalesce($5, max_players), \
             join_policy = coalesce($6, join_policy), \
             join_code = case when coalesce($6, join_policy) = 'open' then null \
                              when join_code is null then $7 else join_code end, \
             updated_at = now() where id = $1 and state <> 'terminated'",
        )
        .bind(id)
        .bind(&u.name)
        .bind(&properties)
        .bind(&data)
        .bind(u.max_players)
        .bind(&u.join_policy)
        .bind(gen_code())
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            let s = self.get(id).await?;
            return Err(Error::Conflict(format!("session is {}", s.state)));
        }
        self.get(id).await
    }

    pub async fn rotate_code(&self, id: Uuid) -> Result<Session> {
        let n = sqlx::query(
            "update musubi.game_sessions set join_code = $2, updated_at = now() \
             where id = $1 and state <> 'terminated' and join_policy <> 'open'",
        )
        .bind(id)
        .bind(gen_code())
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::Conflict(
                "session is open or terminated; no code to rotate".into(),
            ));
        }
        self.get(id).await
    }

    /// Marks the session terminated and every seat left. Returns the session as it was (so the
    /// caller can stop the match/server) or `None` when it was already terminated.
    pub async fn terminate(&self, id: Uuid) -> Result<Option<Session>> {
        let before = self.get(id).await?;
        if before.state == "terminated" {
            return Ok(None);
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query(
            "update musubi.game_sessions set state = 'terminated', terminated_at = now(), updated_at = now() \
             where id = $1 and state <> 'terminated'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "update musubi.game_session_members set state = 'left', left_at = now() \
             where session_id = $1 and state in ('invited', 'reserved', 'joined')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Some(before))
    }

    pub async fn members(&self, id: Uuid) -> Result<Vec<Member>> {
        let now = OffsetDateTime::now_utc();
        let rows = sqlx::query_as::<_, MemberRow>(&format!(
            "select {MEMBER_COLS} from musubi.game_session_members where session_id = $1 order by reserved_at"
        ))
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|r| to_member(r, now)).collect())
    }

    pub async fn member(&self, id: Uuid, account: AccountId) -> Result<Option<Member>> {
        let now = OffsetDateTime::now_utc();
        Ok(sqlx::query_as::<_, MemberRow>(&format!(
            "select {MEMBER_COLS} from musubi.game_session_members where session_id = $1 and account_id = $2"
        ))
        .bind(id)
        .bind(account.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(|r| to_member(r, now)))
    }

    /// Can `account` see connect info / the code: owner, or a member who is not `left`.
    pub async fn is_insider(&self, s: &Session, account: AccountId) -> Result<bool> {
        if s.owner_id == Some(account) {
            return Ok(true);
        }
        Ok(self
            .member(s.id, account)
            .await?
            .is_some_and(|m| m.state != "left"))
    }

    /// Reserve a seat. `code` satisfies an invite-only session; `force` (server/owner) bypasses the
    /// policy. A seat already `joined` is returned as is; a stale or `left` one is re-reserved.
    pub async fn reserve(
        &self,
        id: Uuid,
        account: AccountId,
        code: Option<&str>,
        force: bool,
    ) -> Result<(Session, Member)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let sql = format!(
            "select {} from musubi.game_sessions s where s.id = $1 for update of s",
            cols()
        );
        let s = sqlx::query_as::<_, Row>(&sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?
            .map(to_session)
            .ok_or_else(|| Error::NotFound("session".into()))?;
        if s.state != "active" {
            return Err(Error::Conflict(format!("session is {}", s.state)));
        }
        let now = OffsetDateTime::now_utc();
        let existing = sqlx::query_as::<_, MemberRow>(&format!(
            "select {MEMBER_COLS} from musubi.game_session_members where session_id = $1 and account_id = $2 for update"
        ))
        .bind(id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?
        .map(|r| to_member(r, now));
        if let Some(m) = &existing {
            if m.state == "joined" || (m.state == "reserved" && !m.expired) {
                tx.commit().await.map_err(Error::internal)?;
                return Ok((s, m.clone()));
            }
        }
        let invited = existing.as_ref().is_some_and(|m| m.state == "invited");
        if !force && s.owner_id != Some(account) && !invited {
            match s.join_policy.as_str() {
                "open" => {}
                "invite" => {
                    let ok = match (code.map(normalize_code), s.join_code.as_deref()) {
                        (Some(c), Some(expected)) => ct_eq(&c, expected),
                        _ => false,
                    };
                    if !ok {
                        return Err(Error::PermissionDenied(
                            "session is invite-only: a valid code or invitation is required".into(),
                        ));
                    }
                }
                _ => {
                    return Err(Error::PermissionDenied(
                        "session is closed to new players".into(),
                    ))
                }
            }
        }
        if s.max_players > 0 && s.players >= s.max_players as i64 {
            return Err(Error::ResourceExhausted("session is full".into()));
        }
        let token = gen_token();
        let row = sqlx::query_as::<_, MemberRow>(&format!(
            "insert into musubi.game_session_members (session_id, account_id, state, token) values ($1, $2, 'reserved', $3) \
             on conflict (session_id, account_id) do update set state = 'reserved', token = excluded.token, \
             reserved_at = now(), joined_at = null, left_at = null returning {MEMBER_COLS}"
        ))
        .bind(id)
        .bind(account.0)
        .bind(&token)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let s = self.get(id).await?;
        Ok((s, to_member(row, now)))
    }

    /// Owner/member invites a player: an `invited` seat that bypasses the policy, no token yet.
    pub async fn invite(&self, id: Uuid, who: AccountId) -> Result<Member> {
        let s = self.get(id).await?;
        if s.state != "active" {
            return Err(Error::Conflict(format!("session is {}", s.state)));
        }
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<_, MemberRow>(&format!(
            "insert into musubi.game_session_members (session_id, account_id, state, token) values ($1, $2, 'invited', $3) \
             on conflict (session_id, account_id) do update set state = case when musubi.game_session_members.state = 'left' \
             or musubi.game_session_members.state = 'invited' then 'invited' else musubi.game_session_members.state end, \
             left_at = null returning {MEMBER_COLS}"
        ))
        .bind(id)
        .bind(who.0)
        .bind(gen_token())
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(to_member(row, now))
    }

    /// The game server confirms a seat token: `reserved` (fresh) → `joined`; `joined` is idempotent.
    pub async fn accept(&self, session_id: Uuid, token: &str) -> Result<Member> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<_, MemberRow>(&format!(
            "select {MEMBER_COLS} from musubi.game_session_members where session_id = $1 and token = $2"
        ))
        .bind(session_id)
        .bind(token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(|r| to_member(r, now))
        .ok_or_else(|| Error::Unauthenticated("unknown seat token".into()))?;
        match row.state.as_str() {
            "joined" => return Ok(row),
            "reserved" if !row.expired => {}
            "reserved" => {
                return Err(Error::Unauthenticated(
                    "seat reservation expired; reserve again".into(),
                ))
            }
            st => {
                return Err(Error::Unauthenticated(format!(
                    "seat is {st}; reserve again"
                )))
            }
        }
        let row = sqlx::query_as::<_, MemberRow>(&format!(
            "update musubi.game_session_members set state = 'joined', joined_at = now() \
             where session_id = $1 and account_id = $2 returning {MEMBER_COLS}"
        ))
        .bind(session_id)
        .bind(row.account_id.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(to_member(row, now))
    }

    /// Player left (reported by the server or the player). Returns false when there was no seat.
    pub async fn leave(&self, session_id: Uuid, account: AccountId) -> Result<bool> {
        let n = sqlx::query(
            "update musubi.game_session_members set state = 'left', left_at = now() \
             where session_id = $1 and account_id = $2 and state <> 'left'",
        )
        .bind(session_id)
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn remove_member(&self, session_id: Uuid, account: AccountId) -> Result<bool> {
        let n = sqlx::query(
            "delete from musubi.game_session_members where session_id = $1 and account_id = $2",
        )
        .bind(session_id)
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        Ok(n > 0)
    }

    /// Active sessions where `account` holds a live seat (or owns the session).
    pub async fn mine(&self, account: AccountId) -> Result<Vec<Session>> {
        let sql = format!(
            "select {} from musubi.game_sessions s where s.state <> 'terminated' and (s.owner_id = $1 or exists \
             (select 1 from musubi.game_session_members m where m.session_id = s.id and m.account_id = $1 \
              and m.state in ('invited', 'reserved', 'joined'))) order by s.created_at desc limit 50",
            cols()
        );
        Ok(sqlx::query_as::<_, Row>(&sql)
            .bind(account.0)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?
            .into_iter()
            .map(to_session)
            .collect())
    }

    /// Every session `account` ever held a seat in, newest first (within retention).
    pub async fn history(&self, account: AccountId, limit: i64) -> Result<Vec<Session>> {
        let sql = format!(
            "select {} from musubi.game_sessions s where s.owner_id = $1 or exists \
             (select 1 from musubi.game_session_members m where m.session_id = s.id and m.account_id = $1) \
             order by s.created_at desc limit $2",
            cols()
        );
        Ok(sqlx::query_as::<_, Row>(&sql)
            .bind(account.0)
            .bind(limit.clamp(1, 200))
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?
            .into_iter()
            .map(|r| to_session(r).public())
            .collect())
    }

    pub async fn active_owned(&self, owner: AccountId, template: &str) -> Result<i64> {
        sqlx::query_scalar(
            "select count(*) from musubi.game_sessions where owner_id = $1 and template_id = $2 and state <> 'terminated'",
        )
        .bind(owner.0)
        .bind(template)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)
    }

    /// Property search. `public_only` (players): active, `open`, with free seats — the server
    /// browser; servers see everything (`include_terminated` adds history).
    pub async fn search(&self, req: &SearchSessions, public_only: bool) -> Result<SessionPage> {
        if req.filters.len() > 16 || req.sort.len() > 4 {
            return Err(Error::InvalidArgument(
                "at most 16 filters and 4 sort keys".into(),
            ));
        }
        let limit = req.limit.unwrap_or(50).clamp(1, 100);
        let offset = req
            .offset
            .unwrap_or(0)
            .clamp(0, crate::lobby::MAX_SEARCH_OFFSET);
        let mut sql = format!("select {} from musubi.game_sessions s where true", cols());
        if public_only {
            sql.push_str(" and s.state = 'active' and s.join_policy = 'open'");
        } else if !req.include_terminated {
            sql.push_str(" and s.state <> 'terminated'");
        }
        let mut binds: Vec<Bind> = Vec::new();
        fn arg(binds: &mut Vec<Bind>, b: Bind) -> String {
            binds.push(b);
            format!("${}", binds.len())
        }
        if let Some(r) = &req.region {
            let a = arg(&mut binds, Bind::Str(r.clone()));
            sql.push_str(&format!(" and s.region = {a}"));
        }
        if let Some(k) = &req.kind {
            let a = arg(&mut binds, Bind::Str(k.clone()));
            sql.push_str(&format!(" and s.kind = {a}"));
        }
        if req.has_slots.unwrap_or(public_only) {
            sql.push_str(&format!(
                " and (s.max_players = 0 or {PLAYERS} < s.max_players)"
            ));
        }
        for f in &req.filters {
            let (t, n) = key_exprs(&f.key)?;
            let clause = match f.op.as_str() {
                "exists" => format!("({t} is not null)"),
                "in" => {
                    let vals: Vec<String> = f
                        .value
                        .as_array()
                        .ok_or_else(|| {
                            Error::InvalidArgument(format!("{}: in needs an array", f.key))
                        })?
                        .iter()
                        .map(scalar_text)
                        .collect::<Result<_>>()?;
                    let a = arg(&mut binds, Bind::Strs(vals));
                    format!("({t} = any({a}::text[]))")
                }
                "contains" => {
                    let a = arg(
                        &mut binds,
                        Bind::Str(scalar_text(&f.value)?.replace(['%', '_'], "")),
                    );
                    format!("({t} ilike '%' || {a} || '%')")
                }
                op @ ("eq" | "ne" | "gt" | "gte" | "lt" | "lte") => {
                    let sym = match op {
                        "eq" => "=",
                        "ne" => "<>",
                        "gt" => ">",
                        "gte" => ">=",
                        "lt" => "<",
                        _ => "<=",
                    };
                    match &f.value {
                        Value::Number(x) => {
                            let a = arg(&mut binds, Bind::Num(x.as_f64().unwrap_or(0.0)));
                            format!("({n} {sym} {a}::numeric)")
                        }
                        v if op == "eq" || op == "ne" => {
                            let a = arg(&mut binds, Bind::Str(scalar_text(v)?));
                            if op == "ne" {
                                format!("({t} is null or {t} <> {a})")
                            } else {
                                format!("({t} = {a})")
                            }
                        }
                        _ => {
                            return Err(Error::InvalidArgument(format!(
                                "{}: {op} needs a number",
                                f.key
                            )))
                        }
                    }
                }
                other => {
                    return Err(Error::InvalidArgument(format!(
                        "{}: unknown op {other:?}",
                        f.key
                    )))
                }
            };
            sql.push_str(" and ");
            sql.push_str(&clause);
        }
        let mut order: Vec<String> = Vec::new();
        for so in &req.sort {
            let (t, n) = key_exprs(&so.key)?;
            let dir = match so.dir.as_deref().unwrap_or("asc") {
                "asc" => "asc",
                "desc" => "desc",
                d => return Err(Error::InvalidArgument(format!("sort dir {d:?}"))),
            };
            if let Some(x) = so.near {
                let a = arg(&mut binds, Bind::Num(x));
                order.push(format!("abs({n} - {a}::numeric) {dir} nulls last"));
            } else {
                order.push(format!("{n} {dir} nulls last, {t} {dir} nulls last"));
            }
        }
        order.push("s.created_at desc".into());
        let lim = arg(&mut binds, Bind::Num(limit as f64));
        let off = arg(&mut binds, Bind::Num(offset as f64));
        sql.push_str(&format!(
            " order by {} limit {lim}::numeric::int offset {off}::numeric::int",
            order.join(", ")
        ));
        let mut q = sqlx::query_as::<_, Row>(&sql);
        for b in &binds {
            q = match b {
                Bind::Num(x) => q.bind(*x),
                Bind::Str(s) => q.bind(s.clone()),
                Bind::Strs(v) => q.bind(v.clone()),
            };
        }
        let rows = q.fetch_all(&self.db).await.map_err(|e| {
            tracing::debug!(error = %e, sql, "session search failed");
            Error::InvalidArgument("search failed: unsupported filter or sort expression".into())
        })?;
        let next_cursor = (rows.len() as i64 == limit).then(|| (offset + limit).to_string());
        Ok(SessionPage {
            sessions: rows.into_iter().map(|r| to_session(r).public()).collect(),
            next_cursor,
        })
    }

    // ----- templates -----

    pub async fn put_template(&self, id: &str, mut t: Template) -> Result<Template> {
        if id.is_empty() || id.len() > 64 {
            return Err(Error::InvalidArgument("template id: 1..64 chars".into()));
        }
        if t.params.is_null() {
            t.params = json!({});
        }
        t.validate()?;
        t.id = id.to_string();
        sqlx::query(
            "insert into musubi.game_session_templates (id, name, fleet, region, module, params, max_players, join_policy, \
             max_active_per_owner, wait_secs, enabled) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             on conflict (id) do update set name = excluded.name, fleet = excluded.fleet, region = excluded.region, \
             module = excluded.module, params = excluded.params, max_players = excluded.max_players, \
             join_policy = excluded.join_policy, max_active_per_owner = excluded.max_active_per_owner, \
             wait_secs = excluded.wait_secs, enabled = excluded.enabled, updated_at = now()",
        )
        .bind(&t.id)
        .bind(&t.name)
        .bind(&t.fleet)
        .bind(&t.region)
        .bind(&t.module)
        .bind(&t.params)
        .bind(t.max_players)
        .bind(&t.join_policy)
        .bind(t.max_active_per_owner)
        .bind(t.wait_secs)
        .bind(t.enabled)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(t)
    }

    pub async fn templates(&self) -> Result<Vec<Template>> {
        let rows: Vec<TemplateRow> =
            sqlx::query_as("select id, name, fleet, region, module, params, max_players, join_policy, max_active_per_owner, wait_secs, enabled from musubi.game_session_templates order by id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(Template::from).collect())
    }

    pub async fn template(&self, id: &str) -> Result<Template> {
        let row: Option<TemplateRow> =
            sqlx::query_as("select id, name, fleet, region, module, params, max_players, join_policy, max_active_per_owner, wait_secs, enabled from musubi.game_session_templates where id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        row.map(Template::from)
            .ok_or_else(|| Error::NotFound("session template".into()))
    }

    pub async fn delete_template(&self, id: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.game_session_templates where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }
}

#[derive(sqlx::FromRow)]
struct TemplateRow {
    id: String,
    name: String,
    fleet: Option<String>,
    region: Option<String>,
    module: Option<String>,
    params: Value,
    max_players: i32,
    join_policy: String,
    max_active_per_owner: i32,
    wait_secs: i32,
    enabled: bool,
}

impl From<TemplateRow> for Template {
    fn from(r: TemplateRow) -> Self {
        Template {
            id: r.id,
            name: r.name,
            fleet: r.fleet,
            region: r.region,
            module: r.module,
            params: r.params,
            max_players: r.max_players,
            join_policy: r.join_policy,
            max_active_per_owner: r.max_active_per_owner,
            wait_secs: r.wait_secs,
            enabled: r.enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_tokens() {
        let c = gen_code();
        assert_eq!(c.len(), 6);
        assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize_code(" ab-cd 12 "), "ABCD12");
        assert_eq!(gen_token().len(), 64);
    }

    #[test]
    fn template_validation() {
        let mut t = Template {
            fleet: Some("eu".into()),
            max_players: 4,
            join_policy: "invite".into(),
            max_active_per_owner: 1,
            ..Default::default()
        };
        assert!(t.validate().is_ok());
        t.module = Some("arena".into());
        assert!(t.validate().is_err(), "fleet xor module");
        t.module = None;
        t.max_players = 0;
        assert!(t.validate().is_err());
        t.max_players = 4;
        t.join_policy = "friends".into();
        assert!(t.validate().is_err());
    }

    #[test]
    fn search_keys() {
        assert!(key_exprs("free_slots").is_ok());
        assert!(key_exprs("mode").is_ok());
        assert!(key_exprs("bad key'").is_err());
    }
}
