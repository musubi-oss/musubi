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
const LOBBY_COLS: &str = "l.id, l.name, l.host_id, l.visibility, l.code, l.password_hash, l.max_members, l.team_count, l.team_size, l.region, l.metadata, l.state, l.started, l.created_at, l.updated_at, (select count(*) from musubi.lobby_members m where m.lobby_id = l.id)";

fn to_lobby(r: LobbyRow, member: bool) -> Lobby {
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
        metadata: r.10,
        state: r.11,
        started: r.12,
        member_count: r.15,
        members: Vec::new(),
        created_at: r.13.unix_timestamp(),
        updated_at: r.14.unix_timestamp(),
    }
}

type MemberRow = (Uuid, String, i32, bool, OffsetDateTime);

fn to_member(r: MemberRow) -> Member {
    Member {
        account_id: AccountId(r.0),
        username: r.1,
        team: r.2,
        ready: r.3,
        joined_at: r.4.unix_timestamp(),
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
    // the tail of a v7 uuid is random; 6 symbols of a 32-letter alphabet = 30 bits
    Uuid::now_v7().as_bytes()[10..16]
        .iter()
        .map(|b| CODE_ALPHABET[(*b as usize) % CODE_ALPHABET.len()] as char)
        .collect()
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
            "select account_id, username, team, ready, joined_at from musubi.lobby_members where lobby_id = $1 order by joined_at, account_id",
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
        .bind(q.name.as_deref().map(|s| s.replace(['%', '_'], "")))
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
            "insert into musubi.lobbies (id, name, host_id, visibility, code, password_hash, max_members, team_count, team_size, region, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
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
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        self.get(&id.to_string(), Some(me)).await
    }

    pub async fn join(&self, me: AccountId, username: &str, req: JoinLobby) -> Result<Lobby> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let id = match (&req.id, &req.code) {
            (Some(id), _) => parse_id(id)?,
            (None, Some(code)) => {
                sqlx::query_scalar::<_, Uuid>("select id from musubi.lobbies where code = $1")
                    .bind(code.trim().to_ascii_uppercase())
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
        // private lobbies are entered by code only
        if row.3 == "private" && req.code.is_none() {
            return Err(Error::PermissionDenied(
                "private lobby: join by code".into(),
            ));
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
            .map_err(Error::internal)?;
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
        let metadata = req.metadata.clone().unwrap_or(row.10.clone());
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
        sqlx::query(
            "update musubi.lobbies set name = $2, visibility = $3, code = $4, password_hash = $5, max_members = $6,
                team_count = $7, team_size = $8, region = $9, metadata = $10, updated_at = now(), heartbeat_at = now()
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
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
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
