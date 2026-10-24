//! Lobbies (plan 5.4): waiting rooms of up to [`MAX_MEMBERS`] accounts with team slots, an
//! optional password, a join code for private ones, a filterable public listing and a one-call
//! transition to a match or a dedicated server. State lives in Postgres (row locks make every
//! change cluster-safe); members learn about changes through non-persistent notifications with
//! subject `lobby` and code [`CODE_LOBBY`] whose content carries `event` and `lobby_id`.
//! An account is in at most one lobby at a time. Lobbies whose host stops sending heartbeats
//! for [`TTL`] are swept.

use crate::notify::Notifier;
use musubi_core::{AccountId, Error, Result};
use musubi_social::NewNotification;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_MEMBERS: i32 = 1000;
pub const MAX_TEAMS: i32 = 64;
pub const METADATA_BYTES: usize = 16 * 1024;
/// Notification code of every lobby event.
pub const CODE_LOBBY: i32 = 20;
/// Seconds without a host heartbeat (or any change) after which a lobby is swept.
pub const TTL: i64 = 300;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub account_id: AccountId,
    pub username: String,
    pub team: i32,
    pub ready: bool,
    pub joined_at: i64,
    /// Per-member data (`PUT /v1/lobbies/{id}/me`): loadout, colour, platform …
    pub data: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Lobby {
    pub id: String,
    pub name: String,
    pub host_id: AccountId,
    pub visibility: String,
    /// Join code of a private lobby; only members see it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub has_password: bool,
    pub max_members: i32,
    /// 0 = no team slots (everyone is on team 0).
    pub team_count: i32,
    pub team_size: i32,
    pub region: Option<String>,
    pub metadata: Value,
    /// Typed public attributes (string / number / bool), searchable with `POST /v1/lobbies/search`.
    pub attributes: Value,
    /// Attributes only members see (host sets them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_attributes: Option<Value>,
    pub state: String,
    /// Connect info once started (`start` payload: allocation / match / custom).
    pub started: Option<Value>,
    pub member_count: i64,
    /// Empty in listings.
    pub members: Vec<Member>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CreateLobby {
    pub name: String,
    /// `public` (listed) or `private` (join by code only).
    pub visibility: Option<String>,
    pub password: Option<String>,
    pub max_members: Option<i32>,
    pub team_count: i32,
    pub team_size: i32,
    pub region: Option<String>,
    pub metadata: Value,
    pub attributes: Value,
    pub private_attributes: Value,
    /// Team the host sits on.
    pub team: i32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UpdateLobby {
    pub name: Option<String>,
    pub visibility: Option<String>,
    /// `Some("")` clears the password.
    pub password: Option<String>,
    pub max_members: Option<i32>,
    pub team_count: Option<i32>,
    pub team_size: Option<i32>,
    pub region: Option<String>,
    pub metadata: Option<Value>,
    pub attributes: Option<Value>,
    pub private_attributes: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct JoinLobby {
    pub id: Option<String>,
    pub code: Option<String>,
    pub password: Option<String>,
    pub team: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ListQuery {
    /// Case-insensitive name prefix.
    pub name: Option<String>,
    pub region: Option<String>,
    /// Only lobbies with a free slot (default true).
    pub has_slots: Option<bool>,
    /// JSON containment filter on `metadata` (`{"mode":"ctf"}`), as a JSON string.
    pub metadata: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LobbyPage {
    pub lobbies: Vec<Lobby>,
    pub next_cursor: Option<String>,
}

type LobbyRow = (
    Uuid,
    String,
    Uuid,
    String,
    Option<String>,
    Option<String>,
    i32,
    i32,
    i32,
    Option<String>,
    Value,
    String,
    Option<Value>,
    OffsetDateTime,
    OffsetDateTime,
    i64,
);
const LOBBY_COLS: &str = "l.id, l.name, l.host_id, l.visibility, l.code, l.password_hash, l.max_members, l.team_count, l.team_size, l.region, jsonb_build_object('m', l.metadata, 'a', l.attributes, 'p', l.private_attributes), l.state, l.started, l.created_at, l.updated_at, (select count(*) from musubi.lobby_members m where m.lobby_id = l.id)";

fn to_lobby(r: LobbyRow, member: bool) -> Lobby {
    let packed = r.10;
    Lobby {
        id: r.0.to_string(),
        name: r.1,
        host_id: AccountId(r.2),
        visibility: r.3,
        code: if member { r.4 } else { None },
        has_password: r.5.is_some(),
        max_members: r.6,
        team_count: r.7,
        team_size: r.8,
        region: r.9,
        metadata: packed["m"].clone(),
        attributes: packed["a"].clone(),
        private_attributes: member.then_some(packed["p"].clone()),
        state: r.11,
        started: r.12,
        member_count: r.15,
        members: Vec::new(),
        created_at: r.13.unix_timestamp(),
        updated_at: r.14.unix_timestamp(),
    }
}

type MemberRow = (Uuid, String, i32, bool, OffsetDateTime, Value);

fn to_member(r: MemberRow) -> Member {
    Member {
        account_id: AccountId(r.0),
        username: r.1,
        team: r.2,
        ready: r.3,
        joined_at: r.4.unix_timestamp(),
        data: r.5,
    }
}

fn parse_id(id: &str) -> Result<Uuid> {
    id.parse()
        .map_err(|_| Error::InvalidArgument("bad lobby id".into()))
}

fn validate_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 {
        return Err(Error::InvalidArgument("name must be 1..=64 chars".into()));
    }
    Ok(())
}

fn validate_visibility(v: &str) -> Result<()> {
    if matches!(v, "public" | "private") {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "visibility must be public or private".into(),
        ))
    }
}

fn validate_shape(max_members: i32, team_count: i32, team_size: i32) -> Result<()> {
    if !(2..=MAX_MEMBERS).contains(&max_members) {
        return Err(Error::InvalidArgument(format!(
            "max_members must be 2..={MAX_MEMBERS}"
        )));
    }
    if !(0..=MAX_TEAMS).contains(&team_count) || team_size < 0 {
        return Err(Error::InvalidArgument(format!(
            "team_count must be 0..={MAX_TEAMS}, team_size >= 0"
        )));
    }
    if (team_count == 0) != (team_size == 0) {
        return Err(Error::InvalidArgument(
            "team_count and team_size go together".into(),
        ));
    }
    if team_count > 0 && (team_count as i64) * (team_size as i64) < max_members as i64 {
        return Err(Error::InvalidArgument(
            "team slots must cover max_members".into(),
        ));
    }
    Ok(())
}

fn validate_metadata(v: &Value) -> Result<()> {
    if !v.is_object() {
        return Err(Error::InvalidArgument("metadata must be an object".into()));
    }
    if serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX) > METADATA_BYTES {
        return Err(Error::InvalidArgument(format!(
            "metadata must be <= {METADATA_BYTES} bytes"
        )));
    }
    Ok(())
}

fn team_ok(team: i32, team_count: i32) -> Result<()> {
    if team < 0 || (team_count == 0 && team != 0) || (team_count > 0 && team >= team_count) {
        return Err(Error::InvalidArgument("no such team".into()));
    }
    Ok(())
}

fn gen_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

/// `lobby_members_account_idx` is the last line of defence for "one lobby per account" when two
/// requests race past the check above: report it as a conflict, not as a server error.
fn member_insert_err(e: sqlx::Error) -> Error {
    match e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            Error::Conflict("already in a lobby".into())
        }
        e => Error::internal(e),
    }
}

#[derive(Clone)]
pub struct LobbyService {
    db: PgPool,
    notify: Notifier,
}

impl LobbyService {
    pub fn new(db: PgPool, notify: Notifier) -> Self {
        Self { db, notify }
    }

    async fn lock(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<LobbyRow> {
        sqlx::query_as::<_, LobbyRow>(&format!(
            "select {LOBBY_COLS} from musubi.lobbies l where l.id = $1 for update of l"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("lobby".into()))
    }

    async fn members_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Vec<Member>> {
        let rows: Vec<MemberRow> = sqlx::query_as(
            "select account_id, username, team, ready, joined_at, data from musubi.lobby_members where lobby_id = $1 order by joined_at, account_id",
        )
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_member).collect())
    }

    async fn touch(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
        sqlx::query(
            "update musubi.lobbies set updated_at = now(), heartbeat_at = now() where id = $1",
        )
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    async fn emit(
        &self,
        to: &[AccountId],
        sender: Option<AccountId>,
        event: &str,
        mut body: Value,
    ) {
        let persistent = matches!(event, "started" | "kicked" | "closed");
        if to.is_empty() {
            return;
        }
        body["event"] = json!(event);
        let batch = to
            .iter()
            .map(|&user_id| NewNotification {
                user_id,
                sender_id: sender,
                subject: "lobby".into(),
                content: body.clone(),
                code: CODE_LOBBY,
                persistent,
                ttl_secs: None,
            })
            .collect();
        if let Err(e) = self.notify.send(batch).await {
            tracing::warn!(error = %e, event, "lobby notification failed");
        }
    }

    /// Full view (members, code for members) of one lobby.
    pub async fn get(&self, id: &str, viewer: Option<AccountId>) -> Result<Lobby> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = sqlx::query_as::<_, LobbyRow>(&format!(
            "select {LOBBY_COLS} from musubi.lobbies l where l.id = $1"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("lobby".into()))?;
        let members = Self::members_tx(&mut tx, id).await?;
        tx.commit().await.map_err(Error::internal)?;
        let is_member = viewer.is_some_and(|v| members.iter().any(|m| m.account_id == v));
        let mut lobby = to_lobby(row, is_member);
        lobby.members = members;
        Ok(lobby)
    }

    /// The lobby `me` is in, if any.
    pub async fn mine(&self, me: AccountId) -> Result<Option<Lobby>> {
        let id: Option<Uuid> =
            sqlx::query_scalar("select lobby_id from musubi.lobby_members where account_id = $1")
                .bind(me.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        match id {
            Some(id) => Ok(Some(self.get(&id.to_string(), Some(me)).await?)),
            None => Ok(None),
        }
    }

    /// Public, waiting lobbies, newest first.
    pub async fn list(&self, q: &ListQuery) -> Result<LobbyPage> {
        let limit = q.limit.unwrap_or(50).clamp(1, 100);
        let cursor = q.cursor.as_deref().map(parse_id).transpose()?;
        let metadata: Option<Value> = match &q.metadata {
            Some(s) => Some(
                serde_json::from_str(s)
                    .map_err(|_| Error::InvalidArgument("metadata filter must be JSON".into()))?,
            ),
            None => None,
        };
        let has_slots = q.has_slots.unwrap_or(true);
        let rows: Vec<LobbyRow> = sqlx::query_as(&format!(
            "select {LOBBY_COLS} from musubi.lobbies l
             where l.visibility = 'public' and l.state = 'waiting'
               and ($1::text is null or lower(l.name) like lower($1) || '%')
               and ($2::text is null or l.region = $2)
               and ($3::jsonb is null or l.metadata @> $3)
               and (not $4 or (select count(*) from musubi.lobby_members m where m.lobby_id = l.id) < l.max_members)
               and ($5::uuid is null or l.id < $5)
             order by l.id desc limit $6"
        ))
        .bind(q.name.as_deref().map(|s| s.replace(['%', '_', '\\'], "")))
        .bind(q.region.as_deref())
        .bind(metadata)
        .bind(has_slots)
        .bind(cursor)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let next_cursor = (rows.len() as i64 == limit).then(|| rows.last().unwrap().0.to_string());
        Ok(LobbyPage {
            lobbies: rows.into_iter().map(|r| to_lobby(r, false)).collect(),
            next_cursor,
        })
    }

    pub async fn create(&self, me: AccountId, username: &str, req: CreateLobby) -> Result<Lobby> {
        let attributes = validate_attrs(&req.attributes, "attributes")?;
        let private_attributes = validate_attrs(&req.private_attributes, "private_attributes")?;
        validate_name(&req.name)?;
        let visibility = req.visibility.clone().unwrap_or_else(|| "public".into());
        validate_visibility(&visibility)?;
        let max_members = req.max_members.unwrap_or(if req.team_count > 0 {
            req.team_count * req.team_size
        } else {
            16
        });
        validate_shape(max_members, req.team_count, req.team_size)?;
        let metadata = if req.metadata.is_null() {
            json!({})
        } else {
            req.metadata.clone()
        };
        validate_metadata(&metadata)?;
        team_ok(req.team, req.team_count)?;
        let password_hash = match req.password.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => Some(musubi_auth::password::hash(p.to_string()).await?),
            None => None,
        };
        let id = Uuid::now_v7();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let already: Option<Uuid> =
            sqlx::query_scalar("select lobby_id from musubi.lobby_members where account_id = $1")
                .bind(me.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if already.is_some() {
            return Err(Error::Conflict("already in a lobby".into()));
        }
        let mut code = None;
        if visibility == "private" {
            for _ in 0..8 {
                let c = gen_code();
                let taken: bool = sqlx::query_scalar(
                    "select exists(select 1 from musubi.lobbies where code = $1)",
                )
                .bind(&c)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
                if !taken {
                    code = Some(c);
                    break;
                }
            }
            if code.is_none() {
                return Err(Error::internal("could not allocate a lobby code"));
            }
        }
        sqlx::query(
            "insert into musubi.lobbies (id, name, host_id, visibility, code, password_hash, max_members, team_count, team_size, region, metadata, attributes, private_attributes)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(id)
        .bind(req.name.trim())
        .bind(me.0)
        .bind(&visibility)
        .bind(&code)
        .bind(&password_hash)
        .bind(max_members)
        .bind(req.team_count)
        .bind(req.team_size)
        .bind(req.region.as_deref())
        .bind(&metadata)
        .bind(&attributes)
        .bind(&private_attributes)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query("insert into musubi.lobby_members (lobby_id, account_id, username, team) values ($1, $2, $3, $4)")
            .bind(id)
            .bind(me.0)
            .bind(username)
            .bind(req.team)
            .execute(&mut *tx)
            .await
            .map_err(member_insert_err)?;
        tx.commit().await.map_err(Error::internal)?;
        self.get(&id.to_string(), Some(me)).await
    }

    pub async fn join(&self, me: AccountId, username: &str, req: JoinLobby) -> Result<Lobby> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let id = match (&req.id, &req.code) {
            (Some(id), _) => parse_id(id)?,
            (None, Some(code)) => {
                sqlx::query_scalar::<_, Uuid>("select id from musubi.lobbies where code = $1")
                    .bind(normalize_code(code))
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(Error::internal)?
                    .ok_or_else(|| Error::NotFound("lobby".into()))?
            }
            (None, None) => return Err(Error::InvalidArgument("id or code required".into())),
        };
        let row = Self::lock(&mut tx, id).await?;
        if row.11 != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        // private lobbies are entered by code only — also when the caller knows the id
        if row.3 == "private" {
            let ok = match (req.code.as_deref(), row.4.as_deref()) {
                (Some(c), Some(expected)) => ct_eq(&normalize_code(c), expected),
                _ => false,
            };
            if !ok {
                return Err(Error::PermissionDenied(
                    "private lobby: join by code".into(),
                ));
            }
        }
        if let Some(hash) = &row.5 {
            let ok = match req.password.as_deref() {
                Some(p) => musubi_auth::password::verify(p.to_string(), hash.clone()).await?,
                None => false,
            };
            if !ok {
                return Err(Error::PermissionDenied("wrong lobby password".into()));
            }
        }
        let members = Self::members_tx(&mut tx, id).await?;
        if members.iter().any(|m| m.account_id == me) {
            return Err(Error::Conflict("already in this lobby".into()));
        }
        let elsewhere: Option<Uuid> =
            sqlx::query_scalar("select lobby_id from musubi.lobby_members where account_id = $1")
                .bind(me.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if elsewhere.is_some() {
            return Err(Error::Conflict("already in a lobby".into()));
        }
        if members.len() as i32 >= row.6 {
            return Err(Error::ResourceExhausted("lobby is full".into()));
        }
        let team = match req.team {
            Some(t) => {
                team_ok(t, row.7)?;
                t
            }
            None if row.7 > 0 => (0..row.7)
                .min_by_key(|t| members.iter().filter(|m| m.team == *t).count())
                .unwrap_or(0),
            None => 0,
        };
        if row.7 > 0 && members.iter().filter(|m| m.team == team).count() as i32 >= row.8 {
            return Err(Error::ResourceExhausted("team is full".into()));
        }
        sqlx::query("insert into musubi.lobby_members (lobby_id, account_id, username, team) values ($1, $2, $3, $4)")
            .bind(id)
            .bind(me.0)
            .bind(username)
            .bind(team)
            .execute(&mut *tx)
            .await
            .map_err(member_insert_err)?;
        Self::touch(&mut tx, id).await?;
        tx.commit().await.map_err(Error::internal)?;
        let others: Vec<AccountId> = members.iter().map(|m| m.account_id).collect();
        self.emit(
            &others,
            Some(me),
            "joined",
            json!({ "lobby_id": id, "account_id": me, "username": username, "team": team }),
        )
        .await;
        self.get(&id.to_string(), Some(me)).await
    }

    /// Remove `who` (by themselves or kicked by the host). Host leaving hands the lobby to the
    /// longest-standing member; the last member leaving closes it.
    async fn remove(&self, id: Uuid, who: AccountId, by: AccountId) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        let host = AccountId(row.2);
        if who != by && host != by {
            return Err(Error::PermissionDenied("only the host can kick".into()));
        }
        let n =
            sqlx::query("delete from musubi.lobby_members where lobby_id = $1 and account_id = $2")
                .bind(id)
                .bind(who.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?
                .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("not a member".into()));
        }
        let rest = Self::members_tx(&mut tx, id).await?;
        let mut new_host = None;
        if rest.is_empty() {
            sqlx::query("delete from musubi.lobbies where id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        } else {
            if host == who {
                let h = rest[0].account_id;
                sqlx::query("update musubi.lobbies set host_id = $2 where id = $1")
                    .bind(id)
                    .bind(h.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                new_host = Some(h);
            }
            Self::touch(&mut tx, id).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        let to: Vec<AccountId> = rest.iter().map(|m| m.account_id).collect();
        let event = if who == by { "left" } else { "kicked" };
        self.emit(
            &to,
            Some(by),
            event,
            json!({ "lobby_id": id, "account_id": who, "new_host": new_host }),
        )
        .await;
        if who != by {
            self.emit(
                &[who],
                Some(by),
                "kicked",
                json!({ "lobby_id": id, "account_id": who }),
            )
            .await;
        }
        Ok(())
    }

    pub async fn leave(&self, me: AccountId, id: &str) -> Result<()> {
        self.remove(parse_id(id)?, me, me).await
    }

    pub async fn kick(&self, me: AccountId, id: &str, who: AccountId) -> Result<()> {
        if who == me {
            return Err(Error::InvalidArgument("use leave".into()));
        }
        self.remove(parse_id(id)?, who, me).await
    }

    pub async fn set_ready(&self, me: AccountId, id: &str, ready: bool) -> Result<Lobby> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        if row.11 != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        let n = sqlx::query(
            "update musubi.lobby_members set ready = $3 where lobby_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(me.0)
        .bind(ready)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::PermissionDenied("not a member".into()));
        }
        Self::touch(&mut tx, id).await?;
        let members = Self::members_tx(&mut tx, id).await?;
        tx.commit().await.map_err(Error::internal)?;
        let to: Vec<AccountId> = members
            .iter()
            .map(|m| m.account_id)
            .filter(|a| *a != me)
            .collect();
        self.emit(
            &to,
            Some(me),
            "ready",
            json!({ "lobby_id": id, "account_id": me, "ready": ready }),
        )
        .await;
        self.get(&id.to_string(), Some(me)).await
    }

    pub async fn set_team(&self, me: AccountId, id: &str, team: i32) -> Result<Lobby> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        if row.11 != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        team_ok(team, row.7)?;
        let members = Self::members_tx(&mut tx, id).await?;
        if !members.iter().any(|m| m.account_id == me) {
            return Err(Error::PermissionDenied("not a member".into()));
        }
        if row.7 > 0
            && members
                .iter()
                .filter(|m| m.team == team && m.account_id != me)
                .count() as i32
                >= row.8
        {
            return Err(Error::ResourceExhausted("team is full".into()));
        }
        sqlx::query(
            "update musubi.lobby_members set team = $3 where lobby_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(me.0)
        .bind(team)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        Self::touch(&mut tx, id).await?;
        tx.commit().await.map_err(Error::internal)?;
        let to: Vec<AccountId> = members
            .iter()
            .map(|m| m.account_id)
            .filter(|a| *a != me)
            .collect();
        self.emit(
            &to,
            Some(me),
            "team",
            json!({ "lobby_id": id, "account_id": me, "team": team }),
        )
        .await;
        self.get(&id.to_string(), Some(me)).await
    }

    /// Host-only settings change; shrinking below the current member count is rejected.
    /// Validates and writes a lobby patch inside `tx` (`members` = current roster).
    async fn apply_update(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        row: &LobbyRow,
        members: &[Member],
        req: &UpdateLobby,
    ) -> Result<()> {
        let name = req.name.clone().unwrap_or(row.1.clone());
        validate_name(&name)?;
        let visibility = req.visibility.clone().unwrap_or(row.3.clone());
        validate_visibility(&visibility)?;
        let max_members = req.max_members.unwrap_or(row.6);
        let team_count = req.team_count.unwrap_or(row.7);
        let team_size = req.team_size.unwrap_or(row.8);
        validate_shape(max_members, team_count, team_size)?;
        if (members.len() as i32) > max_members {
            return Err(Error::Conflict("more members than max_members".into()));
        }
        if team_count > 0 {
            for t in 0..team_count {
                if members.iter().filter(|m| m.team == t).count() as i32 > team_size {
                    return Err(Error::Conflict(format!("team {t} would overflow")));
                }
            }
        }
        if members.iter().any(|m| m.team >= team_count.max(1)) {
            return Err(Error::Conflict(
                "a member sits on a team that would disappear".into(),
            ));
        }
        let metadata = req.metadata.clone().unwrap_or(row.10["m"].clone());
        validate_metadata(&metadata)?;
        let password_hash = match req.password.as_deref() {
            None => row.5.clone(),
            Some("") => None,
            Some(p) => Some(musubi_auth::password::hash(p.to_string()).await?),
        };
        let code = match (visibility.as_str(), &row.4) {
            ("private", None) => Some(gen_code()),
            ("public", _) => None,
            (_, c) => c.clone(),
        };
        let region = req.region.clone().or(row.9.clone());
        let attributes = match &req.attributes {
            Some(a) => validate_attrs(a, "attributes")?,
            None => row.10["a"].clone(),
        };
        let private_attributes = match &req.private_attributes {
            Some(a) => validate_attrs(a, "private_attributes")?,
            None => row.10["p"].clone(),
        };
        sqlx::query(
            "update musubi.lobbies set name = $2, visibility = $3, code = $4, password_hash = $5, max_members = $6,
                team_count = $7, team_size = $8, region = $9, metadata = $10, attributes = $11,
                private_attributes = $12, updated_at = now(), heartbeat_at = now()
             where id = $1",
        )
        .bind(id)
        .bind(name.trim())
        .bind(&visibility)
        .bind(&code)
        .bind(&password_hash)
        .bind(max_members)
        .bind(team_count)
        .bind(team_size)
        .bind(region.as_deref())
        .bind(&metadata)
        .bind(&attributes)
        .bind(&private_attributes)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn update(&self, me: AccountId, id: &str, req: UpdateLobby) -> Result<Lobby> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        if AccountId(row.2) != me {
            return Err(Error::PermissionDenied("only the host can update".into()));
        }
        if row.11 != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        let members = Self::members_tx(&mut tx, id).await?;
        Self::apply_update(&mut tx, id, &row, &members, &req).await?;
        tx.commit().await.map_err(Error::internal)?;
        let to: Vec<AccountId> = members
            .iter()
            .map(|m| m.account_id)
            .filter(|a| *a != me)
            .collect();
        self.emit(&to, Some(me), "updated", json!({ "lobby_id": id }))
            .await;
        self.get(&id.to_string(), Some(me)).await
    }

    /// Keeps the lobby alive (any member may call; hosts are expected to every minute or so).
    pub async fn heartbeat(&self, me: AccountId, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let n = sqlx::query(
            "update musubi.lobbies l set heartbeat_at = now() where l.id = $1
               and exists(select 1 from musubi.lobby_members m where m.lobby_id = l.id and m.account_id = $2)",
        )
        .bind(id)
        .bind(me.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("lobby".into()));
        }
        Ok(())
    }

    /// Host closes the lobby; everyone else gets `closed`.
    pub async fn close(&self, me: AccountId, id: &str) -> Result<()> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        if AccountId(row.2) != me {
            return Err(Error::PermissionDenied("only the host can close".into()));
        }
        let members = Self::members_tx(&mut tx, id).await?;
        sqlx::query("delete from musubi.lobbies where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let to: Vec<AccountId> = members
            .iter()
            .map(|m| m.account_id)
            .filter(|a| *a != me)
            .collect();
        self.emit(
            &to,
            Some(me),
            "closed",
            json!({ "lobby_id": id, "reason": "host" }),
        )
        .await;
        Ok(())
    }

    /// Host-only: lobby must be waiting (and everyone ready when `require_ready`). Returns the
    /// lobby with members so the caller can allocate; follow with [`Self::mark_started`].
    pub async fn check_start(&self, me: AccountId, id: &str, require_ready: bool) -> Result<Lobby> {
        let lobby = self.get(id, Some(me)).await?;
        if lobby.host_id != me {
            return Err(Error::PermissionDenied("only the host can start".into()));
        }
        if lobby.state != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        if require_ready {
            if let Some(m) = lobby
                .members
                .iter()
                .find(|m| !m.ready && m.account_id != me)
            {
                return Err(Error::Conflict(format!("{} is not ready", m.username)));
            }
        }
        Ok(lobby)
    }

    /// Flip to `started` exactly once (a lost race returns FailedPrecondition) and tell everyone
    /// where to connect.
    pub async fn mark_started(&self, me: AccountId, id: &str, started: Value) -> Result<Lobby> {
        let uid = parse_id(id)?;
        let n = sqlx::query(
            "update musubi.lobbies set state = 'started', started = $2, updated_at = now(), heartbeat_at = now()
             where id = $1 and state = 'waiting' and host_id = $3",
        )
        .bind(uid)
        .bind(&started)
        .bind(me.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::Conflict(
                "lobby already started or host changed".into(),
            ));
        }
        let lobby = self.get(id, Some(me)).await?;
        let to: Vec<AccountId> = lobby.members.iter().map(|m| m.account_id).collect();
        self.emit(
            &to,
            Some(me),
            "started",
            json!({ "lobby_id": uid, "started": started }),
        )
        .await;
        Ok(lobby)
    }

    /// Drop lobbies nobody kept alive for `ttl_secs`; their members get `closed`.
    pub async fn sweep(&self, ttl_secs: i64) -> Result<u64> {
        let stale: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select l.id, m.account_id from musubi.lobbies l join musubi.lobby_members m on m.lobby_id = l.id
             where l.heartbeat_at < now() - make_interval(secs => $1)",
        )
        .bind(ttl_secs as f64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let n = sqlx::query(
            "delete from musubi.lobbies where heartbeat_at < now() - make_interval(secs => $1)",
        )
        .bind(ttl_secs as f64)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        let mut by_lobby: std::collections::HashMap<Uuid, Vec<AccountId>> = Default::default();
        for (l, a) in stale {
            by_lobby.entry(l).or_default().push(AccountId(a));
        }
        for (l, to) in by_lobby {
            self.emit(
                &to,
                None,
                "closed",
                json!({ "lobby_id": l, "reason": "expired" }),
            )
            .await;
        }
        Ok(n)
    }
}

/// Attributes are a flat object of up to 32 string / number / bool values (keys `[A-Za-z0-9_.-]`).
/// One-call roster + settings mutation for the server (UGS "bulk update" parity): everything
/// applies in one transaction or nothing does.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BulkUpdate {
    pub lobby: Option<UpdateLobby>,
    /// New host (must be a member after adds/removes).
    pub host_id: Option<AccountId>,
    /// Members to add: `[{account_id, team?, data?}]`.
    pub add: Vec<BulkAdd>,
    pub remove: Vec<AccountId>,
    /// Per-member patches keyed by account id.
    pub players: std::collections::HashMap<AccountId, BulkPlayer>,
    /// Skip no-op operations (removing a non-member, adding a member, patching a non-member)
    /// instead of failing the whole call.
    pub ignore_ineffectual_updates: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BulkAdd {
    pub account_id: AccountId,
    pub team: Option<i32>,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BulkPlayer {
    pub team: Option<i32>,
    pub ready: Option<bool>,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BulkResult {
    pub removed: Vec<AccountId>,
    pub added: Vec<AccountId>,
    pub updated: Vec<AccountId>,
    pub skipped: Vec<Value>,
    pub new_host: Option<AccountId>,
}

impl LobbyService {
    /// Applies `req` atomically: removes, lobby settings, adds, member patches, host change,
    /// then a final shape check (capacity / team sizes). `usernames` supplies display names
    /// for added members.
    pub async fn bulk_update(
        &self,
        id: &str,
        req: BulkUpdate,
        usernames: &std::collections::HashMap<AccountId, String>,
    ) -> Result<(Lobby, BulkResult)> {
        let id = parse_id(id)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row = Self::lock(&mut tx, id).await?;
        if row.11 != "waiting" {
            return Err(Error::Conflict("lobby already started".into()));
        }
        let mut res = BulkResult::default();
        let mut members = Self::members_tx(&mut tx, id).await?;
        let before: Vec<AccountId> = members.iter().map(|m| m.account_id).collect();
        let skip = |res: &mut BulkResult, who: AccountId, op: &str, why: &str| {
            res.skipped
                .push(json!({ "account_id": who, "op": op, "reason": why }));
        };
        for who in &req.remove {
            if !members.iter().any(|m| m.account_id == *who) {
                if req.ignore_ineffectual_updates {
                    skip(&mut res, *who, "remove", "not a member");
                    continue;
                }
                return Err(Error::NotFound(format!("{} is not a member", who.0)));
            }
            sqlx::query("delete from musubi.lobby_members where lobby_id = $1 and account_id = $2")
                .bind(id)
                .bind(who.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            members.retain(|m| m.account_id != *who);
            res.removed.push(*who);
        }
        let mut row = row;
        if let Some(patch) = &req.lobby {
            Self::apply_update(&mut tx, id, &row, &members, patch).await?;
            row = Self::lock(&mut tx, id).await?;
        }
        for a in &req.add {
            if members.iter().any(|m| m.account_id == a.account_id) {
                if req.ignore_ineffectual_updates {
                    skip(&mut res, a.account_id, "add", "already a member");
                    continue;
                }
                return Err(Error::Conflict(format!(
                    "{} is already in this lobby",
                    a.account_id.0
                )));
            }
            let elsewhere: Option<Uuid> = sqlx::query_scalar(
                "select lobby_id from musubi.lobby_members where account_id = $1",
            )
            .bind(a.account_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if elsewhere.is_some() {
                return Err(Error::Conflict(format!(
                    "{} is already in another lobby",
                    a.account_id.0
                )));
            }
            let team = match a.team {
                Some(t) => {
                    team_ok(t, row.7)?;
                    t
                }
                None if row.7 > 0 => (0..row.7)
                    .min_by_key(|t| members.iter().filter(|m| m.team == *t).count())
                    .unwrap_or(0),
                None => 0,
            };
            let data = match &a.data {
                Some(d) if d.is_object() && d.to_string().len() <= 8192 => d.clone(),
                Some(_) => {
                    return Err(Error::InvalidArgument(
                        "data must be an object up to 8 KiB".into(),
                    ))
                }
                None => json!({}),
            };
            let username = usernames
                .get(&a.account_id)
                .cloned()
                .unwrap_or_else(|| a.account_id.0.to_string());
            sqlx::query(
                "insert into musubi.lobby_members (lobby_id, account_id, username, team, data) values ($1, $2, $3, $4, $5)",
            )
            .bind(id)
            .bind(a.account_id.0)
            .bind(&username)
            .bind(team)
            .bind(&data)
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                    Error::NotFound(format!("account {}", a.account_id.0))
                }
                e => Error::internal(e),
            })?;
            members.push(Member {
                account_id: a.account_id,
                username,
                team,
                ready: false,
                joined_at: OffsetDateTime::now_utc().unix_timestamp(),
                data,
            });
            res.added.push(a.account_id);
        }
        for (who, p) in &req.players {
            let Some(m) = members.iter_mut().find(|m| m.account_id == *who) else {
                if req.ignore_ineffectual_updates {
                    skip(&mut res, *who, "update", "not a member");
                    continue;
                }
                return Err(Error::NotFound(format!("{} is not a member", who.0)));
            };
            if let Some(t) = p.team {
                team_ok(t, row.7)?;
                m.team = t;
            }
            if let Some(r) = p.ready {
                m.ready = r;
            }
            if let Some(d) = &p.data {
                if !d.is_object() || d.to_string().len() > 8192 {
                    return Err(Error::InvalidArgument(
                        "data must be an object up to 8 KiB".into(),
                    ));
                }
                m.data = d.clone();
            }
            sqlx::query(
                "update musubi.lobby_members set team = $3, ready = $4, data = $5 where lobby_id = $1 and account_id = $2",
            )
            .bind(id)
            .bind(who.0)
            .bind(m.team)
            .bind(m.ready)
            .bind(&m.data)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            res.updated.push(*who);
        }
        // shape after everything
        if members.len() as i32 > row.6 {
            return Err(Error::Conflict("more members than max_members".into()));
        }
        if row.7 > 0 {
            for t in 0..row.7 {
                if members.iter().filter(|m| m.team == t).count() as i32 > row.8 {
                    return Err(Error::Conflict(format!("team {t} would overflow")));
                }
            }
        }
        if members.is_empty() {
            sqlx::query("delete from musubi.lobbies where id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            tx.commit().await.map_err(Error::internal)?;
            self.emit(&before, None, "closed", json!({ "lobby_id": id }))
                .await;
            return Err(Error::NotFound(
                "lobby closed: no members left after the update".into(),
            ));
        }
        let host = AccountId(row.2);
        let new_host = match req.host_id {
            Some(h) => {
                if !members.iter().any(|m| m.account_id == h) {
                    return Err(Error::InvalidArgument(
                        "host_id must be a member after the update".into(),
                    ));
                }
                (h != host).then_some(h)
            }
            None if !members.iter().any(|m| m.account_id == host) => Some(members[0].account_id),
            None => None,
        };
        if let Some(h) = new_host {
            sqlx::query("update musubi.lobbies set host_id = $2 where id = $1")
                .bind(id)
                .bind(h.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            res.new_host = Some(h);
        }
        Self::touch(&mut tx, id).await?;
        tx.commit().await.map_err(Error::internal)?;
        let roster: Vec<AccountId> = members.iter().map(|m| m.account_id).collect();
        self.emit(
            &roster,
            None,
            "updated",
            json!({ "lobby_id": id, "bulk": true, "added": res.added, "removed": res.removed,
                "updated": res.updated, "new_host": res.new_host }),
        )
        .await;
        if !res.removed.is_empty() {
            self.emit(&res.removed, None, "kicked", json!({ "lobby_id": id }))
                .await;
        }
        let viewer = res.new_host.unwrap_or(host);
        let lobby = self.get(&id.to_string(), Some(viewer)).await?;
        Ok((lobby, res))
    }
}

pub fn validate_attrs(v: &Value, what: &str) -> Result<Value> {
    if v.is_null() {
        return Ok(json!({}));
    }
    let obj = v
        .as_object()
        .ok_or_else(|| Error::InvalidArgument(format!("{what} must be an object")))?;
    if obj.len() > 32 {
        return Err(Error::InvalidArgument(format!("{what}: at most 32 keys")));
    }
    for (k, val) in obj {
        if !valid_key(k) {
            return Err(Error::InvalidArgument(format!(
                "{what}: key {k:?} must be 1..64 chars of [A-Za-z0-9_.-]"
            )));
        }
        match val {
            Value::String(s) if s.len() <= 512 => {}
            Value::Number(_) | Value::Bool(_) => {}
            Value::String(_) => {
                return Err(Error::InvalidArgument(format!(
                    "{what}.{k}: string too long (512)"
                )))
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "{what}.{k}: values must be string, number or bool"
                )))
            }
        }
    }
    Ok(v.clone())
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// One search predicate on a public attribute or a built-in field.
#[derive(Debug, Clone, Deserialize)]
pub struct Filter {
    /// Attribute key, or built-in `name`, `region`, `member_count`, `max_members`, `free_slots`,
    /// `team_count`, `team_size`, `created_at`, `host_id`.
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
    /// Order by distance from this number (closest first) instead of the raw value.
    #[serde(default)]
    pub near: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SearchLobbies {
    pub filters: Vec<Filter>,
    pub sort: Vec<Sort>,
    pub region: Option<String>,
    pub has_slots: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

enum Bind {
    Num(f64),
    Str(String),
    Strs(Vec<String>),
}

const MEMBERS: &str = "(select count(*) from musubi.lobby_members m where m.lobby_id = l.id)";
/// Deep offset paging is O(offset) on the server; clients page with `next_cursor`.
pub const MAX_SEARCH_OFFSET: i64 = 10_000;

/// (text expression, numeric expression) for a key.
fn key_exprs(key: &str) -> Result<(String, String)> {
    let builtin: Option<(String, bool)> = match key {
        "name" => Some(("l.name".into(), false)),
        "region" => Some(("l.region".into(), false)),
        "host_id" => Some(("l.host_id::text".into(), false)),
        "member_count" => Some((MEMBERS.into(), true)),
        "free_slots" => Some((format!("(l.max_members - {MEMBERS})"), true)),
        "max_members" => Some(("l.max_members".into(), true)),
        "team_count" => Some(("l.team_count".into(), true)),
        "team_size" => Some(("l.team_size".into(), true)),
        "created_at" => Some(("extract(epoch from l.created_at)".into(), true)),
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
        return Err(Error::InvalidArgument(format!("bad attribute key {key:?}")));
    }
    Ok((
        format!("(l.attributes->>'{key}')"),
        format!("case when jsonb_typeof(l.attributes->'{key}') = 'number' then (l.attributes->>'{key}')::numeric end"),
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

impl LobbyService {
    /// Attribute search over public waiting lobbies: typed predicates, sorting (optionally by
    /// distance to a number), offset paging.
    pub async fn search(&self, req: &SearchLobbies) -> Result<LobbyPage> {
        if req.filters.len() > 16 || req.sort.len() > 4 {
            return Err(Error::InvalidArgument(
                "at most 16 filters and 4 sort keys".into(),
            ));
        }
        let limit = req.limit.unwrap_or(50).clamp(1, 100);
        let offset = req.offset.unwrap_or(0).clamp(0, MAX_SEARCH_OFFSET);
        let mut sql = format!(
            "select {LOBBY_COLS} from musubi.lobbies l where l.visibility = 'public' and l.state = 'waiting'"
        );
        let mut binds: Vec<Bind> = Vec::new();
        fn arg(binds: &mut Vec<Bind>, b: Bind) -> String {
            binds.push(b);
            format!("${}", binds.len())
        }
        if let Some(r) = &req.region {
            let a = arg(&mut binds, Bind::Str(r.clone()));
            sql.push_str(&format!(" and l.region = {a}"));
        }
        if req.has_slots.unwrap_or(true) {
            sql.push_str(&format!(" and {MEMBERS} < l.max_members"));
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
        for s in &req.sort {
            let (t, n) = key_exprs(&s.key)?;
            let dir = match s.dir.as_deref().unwrap_or("asc") {
                "asc" => "asc",
                "desc" => "desc",
                d => return Err(Error::InvalidArgument(format!("sort dir {d:?}"))),
            };
            if let Some(x) = s.near {
                let a = arg(&mut binds, Bind::Num(x));
                order.push(format!("abs({n} - {a}::numeric) {dir} nulls last"));
            } else {
                order.push(format!("{n} {dir} nulls last, {t} {dir} nulls last"));
            }
        }
        order.push("l.id desc".into());
        let lim = arg(&mut binds, Bind::Num(limit as f64));
        let off = arg(&mut binds, Bind::Num(offset as f64));
        sql.push_str(&format!(
            " order by {} limit {lim}::numeric::int offset {off}::numeric::int",
            order.join(", ")
        ));
        let mut q = sqlx::query_as::<_, LobbyRow>(&sql);
        for b in &binds {
            q = match b {
                Bind::Num(x) => q.bind(*x),
                Bind::Str(s) => q.bind(s.clone()),
                Bind::Strs(v) => q.bind(v.clone()),
            };
        }
        let rows = q.fetch_all(&self.db).await.map_err(|e| {
            tracing::debug!(error = %e, sql, "lobby search failed");
            Error::InvalidArgument("search failed: unsupported filter or sort expression".into())
        })?;
        let next_cursor = (rows.len() as i64 == limit).then(|| (offset + limit).to_string());
        Ok(LobbyPage {
            lobbies: rows.into_iter().map(|r| to_lobby(r, false)).collect(),
            next_cursor,
        })
    }

    /// A member's own data blob (≤ 8 KiB object); other members get an `updated` event.
    pub async fn set_member_data(&self, me: AccountId, id: &str, data: Value) -> Result<Lobby> {
        if !data.is_object() || data.to_string().len() > 8192 {
            return Err(Error::InvalidArgument(
                "data must be an object up to 8 KiB".into(),
            ));
        }
        let lid = parse_id(id)?;
        let n = sqlx::query(
            "update musubi.lobby_members set data = $3 where lobby_id = $1 and account_id = $2",
        )
        .bind(lid)
        .bind(me.0)
        .bind(&data)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("not a member of this lobby".into()));
        }
        let lobby = self.get(id, Some(me)).await?;
        let to: Vec<AccountId> = lobby
            .members
            .iter()
            .map(|m| m.account_id)
            .filter(|a| *a != me)
            .collect();
        self.emit(
            &to,
            Some(me),
            "updated",
            json!({ "lobby_id": lid, "member": me.0 }),
        )
        .await;
        Ok(lobby)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_six_symbols_of_the_alphabet() {
        for _ in 0..100 {
            let c = gen_code();
            assert_eq!(c.len(), 6);
            assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)), "{c}");
        }
        assert_eq!(normalize_code(" ab2c3d \n"), "AB2C3D");
        assert!(ct_eq("ABCDEF", "ABCDEF"));
        assert!(!ct_eq("ABCDEF", "ABCDEG"));
        assert!(!ct_eq("ABCDE", "ABCDEF"));
    }
}
