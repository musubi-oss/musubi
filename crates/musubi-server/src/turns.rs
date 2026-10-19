//! Asynchronous turn-based matches: correspondence-style games (chess, word games, card duels)
//! where players move on their own schedule. A match is a row in `musubi.turn_matches` holding
//! the seats, the shared `board` document and an optimistic `version`; every move is appended to
//! `musubi.turn_moves` (full history / replay). Turn order is round-robin over the seats that
//! are still active unless the mover names `next_seat`. Each turn has a deadline: the sweeper
//! applies `timeout_action` (`skip` the turn — after `max_skips` in a row the seat resigns — or
//! `resign` straight away). Players learn about changes through non-persistent notifications
//! with subject `turn` and code [`CODE_TURN`] (`event`: invited / joined / started / your_turn /
//! moved / finished / cancelled); the matching `turn_match.*` bus events feed webhooks.

use crate::notify::Notifier;
use musubi_core::{AccountId, Error, Result};
use musubi_social::NewNotification;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub const CODE_TURN: i32 = 40;
pub const MAX_PLAYERS: i32 = 16;
pub const BOARD_BYTES: usize = 64 * 1024;
pub const MOVE_BYTES: usize = 16 * 1024;
pub const METADATA_BYTES: usize = 16 * 1024;
/// Seconds an `open` match waits for players before it is cancelled.
pub const OPEN_TTL: i64 = 86_400;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub account_id: AccountId,
    pub username: String,
    pub seat: i32,
    /// invited | active | left | resigned | timed_out
    pub status: String,
    pub joined_at: i64,
    #[serde(default)]
    pub skips: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

impl Player {
    fn active(&self) -> bool {
        self.status == "active"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Move {
    pub turn: i32,
    pub seat: i32,
    pub account_id: Option<AccountId>,
    /// move | skip | timeout | resign
    pub kind: String,
    pub data: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnMatch {
    pub id: Uuid,
    pub game: String,
    /// open | active | finished | cancelled
    pub state: String,
    pub visibility: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub created_by: AccountId,
    pub players: Vec<Player>,
    pub min_players: i32,
    pub max_players: i32,
    pub turn: i32,
    pub current_seat: Option<i32>,
    pub deadline_at: Option<i64>,
    pub turn_secs: i64,
    pub timeout_action: String,
    pub max_skips: i32,
    pub auto_start: bool,
    pub board: Value,
    pub version: i64,
    pub metadata: Value,
    pub winners: Vec<i32>,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rematch_of: Option<Uuid>,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// Filled by `get` only (`since_turn` filter), empty in listings.
    pub moves: Vec<Move>,
}

impl TurnMatch {
    pub fn seat_of(&self, a: AccountId) -> Option<&Player> {
        self.players.iter().find(|p| p.account_id == a)
    }
    fn active_seats(&self) -> Vec<i32> {
        self.players
            .iter()
            .filter(|p| p.active())
            .map(|p| p.seat)
            .collect()
    }
    /// Next active seat after `from` in round-robin order.
    fn next_seat_after(&self, from: i32) -> Option<i32> {
        let mut seats = self.active_seats();
        seats.sort();
        seats.iter().find(|&&s| s > from).or(seats.first()).copied()
    }
    fn current_player(&self) -> Option<&Player> {
        let s = self.current_seat?;
        self.players.iter().find(|p| p.seat == s)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CreateMatch {
    pub game: String,
    pub min_players: Option<i32>,
    pub max_players: Option<i32>,
    pub invite: Vec<AccountId>,
    pub visibility: Option<String>,
    pub turn_secs: Option<i64>,
    pub timeout_action: Option<String>,
    pub max_skips: Option<i32>,
    pub auto_start: Option<bool>,
    pub board: Value,
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct JoinMatch {
    pub id: Option<Uuid>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Finish {
    pub winners: Vec<i32>,
    pub result: Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MakeMove {
    pub data: Value,
    /// Replacement board document; omitted = unchanged.
    pub board: Option<Value>,
    /// Optimistic concurrency: must equal the match's current `version`.
    pub version: Option<i64>,
    /// Hand the turn to a specific active seat instead of round-robin.
    pub next_seat: Option<i32>,
    /// End the match with this move.
    pub finish: Option<Finish>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ListQuery {
    pub game: Option<String>,
    pub state: Option<String>,
    pub account_id: Option<AccountId>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GetQuery {
    pub since_turn: Option<i32>,
}

#[derive(Clone)]
pub struct TurnService {
    db: PgPool,
    notify: Notifier,
    events: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::events::Events>>>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    game: String,
    state: String,
    visibility: String,
    code: Option<String>,
    created_by: Uuid,
    players: Value,
    min_players: i32,
    max_players: i32,
    turn: i32,
    current_seat: Option<i32>,
    deadline_at: Option<OffsetDateTime>,
    turn_secs: i64,
    timeout_action: String,
    max_skips: i32,
    auto_start: bool,
    board: Value,
    version: i64,
    metadata: Value,
    winners: Value,
    result: Value,
    rematch_of: Option<Uuid>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
}

const COLS: &str = "id, game, state, visibility, code, created_by, players, min_players, max_players, turn, \
    current_seat, deadline_at, turn_secs, timeout_action, max_skips, auto_start, board, version, metadata, \
    winners, result, rematch_of, created_at, updated_at, started_at, finished_at";

fn ts(t: OffsetDateTime) -> i64 {
    t.unix_timestamp()
}

impl From<Row> for TurnMatch {
    fn from(r: Row) -> Self {
        TurnMatch {
            id: r.id,
            game: r.game,
            state: r.state,
            visibility: r.visibility,
            code: r.code,
            created_by: r.created_by.into(),
            players: serde_json::from_value(r.players).unwrap_or_default(),
            min_players: r.min_players,
            max_players: r.max_players,
            turn: r.turn,
            current_seat: r.current_seat,
            deadline_at: r.deadline_at.map(ts),
            turn_secs: r.turn_secs,
            timeout_action: r.timeout_action,
            max_skips: r.max_skips,
            auto_start: r.auto_start,
            board: r.board,
            version: r.version,
            metadata: r.metadata,
            winners: serde_json::from_value(r.winners).unwrap_or_default(),
            result: r.result,
            rematch_of: r.rematch_of,
            created_at: ts(r.created_at),
            updated_at: ts(r.updated_at),
            started_at: r.started_at.map(ts),
            finished_at: r.finished_at.map(ts),
            moves: Vec::new(),
        }
    }
}

fn valid_game(g: &str) -> bool {
    (1..=32).contains(&g.len())
        && g.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn size_ok(v: &Value, max: usize, what: &str) -> Result<()> {
    if v.to_string().len() > max {
        return Err(Error::InvalidArgument(format!(
            "{what} exceeds {max} bytes"
        )));
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

impl TurnService {
    pub fn new(db: PgPool, notify: Notifier) -> Self {
        Self {
            db,
            notify,
            events: Default::default(),
        }
    }

    pub fn bind_events(&self, ev: std::sync::Arc<crate::events::Events>) {
        let _ = self.events.set(ev);
    }

    async fn emit(&self, kind: &str, account: Option<AccountId>, payload: Value) {
        if let Some(ev) = self.events.get() {
            ev.emit(kind, account, payload).await;
        }
    }

    async fn notify_players(
        &self,
        m: &TurnMatch,
        to: &[AccountId],
        event: &str,
        sender: Option<AccountId>,
        extra: Value,
    ) {
        if to.is_empty() {
            return;
        }
        let mut body = json!({
            "event": event,
            "match_id": m.id,
            "game": m.game,
            "state": m.state,
            "turn": m.turn,
            "current_seat": m.current_seat,
            "deadline_at": m.deadline_at,
            "version": m.version,
        });
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                body[k] = v.clone();
            }
        }
        let batch = to
            .iter()
            .map(|&user_id| NewNotification {
                user_id,
                sender_id: sender,
                subject: "turn".into(),
                content: body.clone(),
                code: CODE_TURN,
                persistent: false,
                ttl_secs: None,
            })
            .collect();
        if let Err(e) = self.notify.send(batch).await {
            tracing::warn!(error = %e, event, "turn notification failed");
        }
    }

    fn everyone(m: &TurnMatch) -> Vec<AccountId> {
        m.players
            .iter()
            .filter(|p| p.status != "left")
            .map(|p| p.account_id)
            .collect()
    }

    async fn lock(&self, tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<TurnMatch> {
        let r: Option<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.turn_matches where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        r.map(TurnMatch::from)
            .ok_or_else(|| Error::NotFound(format!("turn match {id}")))
    }

    async fn save(&self, tx: &mut Transaction<'_, Postgres>, m: &TurnMatch) -> Result<()> {
        sqlx::query(
            "update musubi.turn_matches set state = $2, players = $3, turn = $4, current_seat = $5, \
             deadline_at = to_timestamp($6), board = $7, version = $8, winners = $9, result = $10, \
             updated_at = now(), started_at = coalesce(started_at, to_timestamp($11)), \
             finished_at = coalesce(finished_at, to_timestamp($12)), code = $13 where id = $1",
        )
        .bind(m.id)
        .bind(&m.state)
        .bind(serde_json::to_value(&m.players).map_err(Error::internal)?)
        .bind(m.turn)
        .bind(m.current_seat)
        .bind(m.deadline_at.map(|d| d as f64))
        .bind(&m.board)
        .bind(m.version)
        .bind(serde_json::to_value(&m.winners).map_err(Error::internal)?)
        .bind(&m.result)
        .bind(m.started_at.map(|d| d as f64))
        .bind(m.finished_at.map(|d| d as f64))
        .bind(&m.code)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    async fn record_move(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        m: &TurnMatch,
        seat: i32,
        account: Option<AccountId>,
        kind: &str,
        data: &Value,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.turn_moves (match_id, turn, seat, account_id, kind, data) \
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(m.id)
        .bind(m.turn)
        .bind(seat)
        .bind(account.map(|a| a.0))
        .bind(kind)
        .bind(data)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    fn validate_create(req: &CreateMatch) -> Result<(i32, i32, String, i64, String, i32)> {
        if !valid_game(&req.game) {
            return Err(Error::InvalidArgument(
                "game: 1..=32 chars of [a-z0-9_-]".into(),
            ));
        }
        let max = req.max_players.unwrap_or(2);
        let min = req.min_players.unwrap_or(max.min(2));
        if !(2..=MAX_PLAYERS).contains(&max) || min < 2 || min > max {
            return Err(Error::InvalidArgument(format!(
                "players: 2 <= min_players <= max_players <= {MAX_PLAYERS}"
            )));
        }
        if req.invite.len() as i32 + 1 > max {
            return Err(Error::InvalidArgument(
                "invite: more invitees than seats".into(),
            ));
        }
        let vis = req.visibility.clone().unwrap_or_else(|| "public".into());
        if !matches!(vis.as_str(), "public" | "private") {
            return Err(Error::InvalidArgument(
                "visibility must be public or private".into(),
            ));
        }
        let turn_secs = req.turn_secs.unwrap_or(86_400);
        if !(10..=30 * 86_400).contains(&turn_secs) {
            return Err(Error::InvalidArgument(
                "turn_secs must be 10..=2592000".into(),
            ));
        }
        let action = req.timeout_action.clone().unwrap_or_else(|| "skip".into());
        if !matches!(action.as_str(), "skip" | "resign") {
            return Err(Error::InvalidArgument(
                "timeout_action must be skip or resign".into(),
            ));
        }
        let max_skips = req.max_skips.unwrap_or(3);
        if !(1..=100).contains(&max_skips) {
            return Err(Error::InvalidArgument("max_skips must be 1..=100".into()));
        }
        size_ok(&req.board, BOARD_BYTES, "board")?;
        if !req.metadata.is_null() && !req.metadata.is_object() {
            return Err(Error::InvalidArgument("metadata must be an object".into()));
        }
        size_ok(&req.metadata, METADATA_BYTES, "metadata")?;
        Ok((min, max, vis, turn_secs, action, max_skips))
    }

    async fn usernames(&self, ids: &[AccountId]) -> Result<Vec<(AccountId, String)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let raw: Vec<Uuid> = ids.iter().map(|a| a.0).collect();
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("select id, username from musubi.accounts where id = any($1)")
                .bind(&raw)
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|(id, u)| (id.into(), u)).collect())
    }

    pub async fn create(
        &self,
        me: AccountId,
        username: &str,
        req: CreateMatch,
    ) -> Result<TurnMatch> {
        let (min, max, vis, turn_secs, action, max_skips) = Self::validate_create(&req)?;
        let mut invite = req.invite.clone();
        invite.dedup();
        if invite.contains(&me) || invite.len() != req.invite.len() {
            return Err(Error::InvalidArgument(
                "invite: duplicate or self invitation".into(),
            ));
        }
        let known = self.usernames(&invite).await?;
        if known.len() != invite.len() {
            return Err(Error::NotFound("invite: unknown account".into()));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut players = vec![Player {
            account_id: me,
            username: username.to_string(),
            seat: 0,
            status: "active".into(),
            joined_at: now,
            skips: 0,
            score: None,
        }];
        for (i, (id, name)) in known.into_iter().enumerate() {
            players.push(Player {
                account_id: id,
                username: name,
                seat: i as i32 + 1,
                status: "invited".into(),
                joined_at: now,
                skips: 0,
                score: None,
            });
        }
        let id = Uuid::now_v7();
        let code = (vis == "private").then(gen_code);
        let metadata = if req.metadata.is_null() {
            json!({})
        } else {
            req.metadata.clone()
        };
        sqlx::query(
            "insert into musubi.turn_matches (id, game, state, visibility, code, created_by, players, \
             min_players, max_players, turn_secs, timeout_action, max_skips, auto_start, board, metadata) \
             values ($1, $2, 'open', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(&req.game)
        .bind(&vis)
        .bind(&code)
        .bind(me.0)
        .bind(serde_json::to_value(&players).map_err(Error::internal)?)
        .bind(min)
        .bind(max)
        .bind(turn_secs)
        .bind(&action)
        .bind(max_skips)
        .bind(req.auto_start.unwrap_or(true))
        .bind(&req.board)
        .bind(&metadata)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let m = self.fetch(id).await?;
        let invited: Vec<AccountId> = m
            .players
            .iter()
            .filter(|p| p.status == "invited")
            .map(|p| p.account_id)
            .collect();
        self.notify_players(
            &m,
            &invited,
            "invited",
            Some(me),
            json!({ "from": username }),
        )
        .await;
        self.emit(
            "turn_match.created",
            Some(me),
            json!({ "match_id": id, "game": m.game, "invited": invited.len() }),
        )
        .await;
        Ok(m)
    }

    async fn fetch(&self, id: Uuid) -> Result<TurnMatch> {
        let r: Option<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.turn_matches where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        r.map(TurnMatch::from)
            .ok_or_else(|| Error::NotFound(format!("turn match {id}")))
    }

    /// Full match for a player (or the server when `viewer` is `None`): private matches are
    /// visible to their players only; `since_turn` trims the move history.
    pub async fn get(
        &self,
        id: Uuid,
        viewer: Option<AccountId>,
        q: &GetQuery,
    ) -> Result<TurnMatch> {
        let mut m = self.fetch(id).await?;
        let is_player = viewer.is_some_and(|v| m.seat_of(v).is_some());
        if viewer.is_some() && m.visibility == "private" && !is_player {
            return Err(Error::NotFound(format!("turn match {id}")));
        }
        if viewer.is_some() && !is_player {
            m.code = None;
        }
        let since = q.since_turn.unwrap_or(0);
        let rows: Vec<(i32, i32, Option<Uuid>, String, Value, OffsetDateTime)> = sqlx::query_as(
            "select turn, seat, account_id, kind, data, created_at from musubi.turn_moves \
             where match_id = $1 and turn > $2 order by turn",
        )
        .bind(id)
        .bind(since)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        m.moves = rows
            .into_iter()
            .map(|(turn, seat, account_id, kind, data, at)| Move {
                turn,
                seat,
                account_id: account_id.map(AccountId::from),
                kind,
                data,
                created_at: ts(at),
            })
            .collect();
        Ok(m)
    }

    /// Public open matches waiting for players.
    pub async fn list_open(&self, game: Option<&str>, limit: i64) -> Result<Vec<TurnMatch>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.turn_matches where state = 'open' and visibility = 'public' \
             and ($1::text is null or game = $1) order by created_at limit $2"
        ))
        .bind(game)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(TurnMatch::from).collect())
    }

    /// Matches of an account (invitations included) — `state` filter optional; `my_turn` marks
    /// matches waiting on this account.
    pub async fn mine(&self, me: AccountId, state: Option<&str>, limit: i64) -> Result<Vec<Value>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.turn_matches where players @> $1::jsonb \
             and ($2::text is null or state = $2) order by updated_at desc limit $3"
        ))
        .bind(json!([{ "account_id": me }]))
        .bind(state)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(TurnMatch::from)
            .map(|m| {
                let mine = m.seat_of(me).cloned();
                let my_turn = m.state == "active"
                    && mine
                        .as_ref()
                        .is_some_and(|p| Some(p.seat) == m.current_seat);
                let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
                v["my_turn"] = json!(my_turn);
                v["my_seat"] = json!(mine.as_ref().map(|p| p.seat));
                v["my_status"] = json!(mine.as_ref().map(|p| p.status.clone()));
                v
            })
            .collect())
    }

    /// Server-side listing with filters.
    pub async fn list(&self, q: &ListQuery) -> Result<Vec<TurnMatch>> {
        if let Some(s) = &q.state {
            if !matches!(s.as_str(), "open" | "active" | "finished" | "cancelled") {
                return Err(Error::InvalidArgument(
                    "state must be open|active|finished|cancelled".into(),
                ));
            }
        }
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.turn_matches where ($1::text is null or game = $1) \
             and ($2::text is null or state = $2) \
             and ($3::jsonb is null or players @> $3) order by updated_at desc limit $4"
        ))
        .bind(&q.game)
        .bind(&q.state)
        .bind(q.account_id.map(|a| json!([{ "account_id": a }])))
        .bind(q.limit.unwrap_or(50).clamp(1, 500))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(TurnMatch::from).collect())
    }

    fn start_in_place(m: &mut TurnMatch, now: i64) {
        m.players.retain(|p| p.status != "invited");
        for (i, p) in m.players.iter_mut().enumerate() {
            p.seat = i as i32;
        }
        m.state = "active".into();
        m.turn = 1;
        m.current_seat = Some(0);
        m.deadline_at = Some(now + m.turn_secs);
        m.started_at = Some(now);
        m.version += 1;
        m.code = None;
    }

    async fn announce_start(&self, m: &TurnMatch) {
        let all = Self::everyone(m);
        self.notify_players(m, &all, "started", None, Value::Null)
            .await;
        if let Some(p) = m.current_player() {
            self.notify_players(m, &[p.account_id], "your_turn", None, Value::Null)
                .await;
        }
        self.emit(
            "turn_match.started",
            None,
            json!({ "match_id": m.id, "game": m.game, "players": m.players.len() }),
        )
        .await;
    }

    pub async fn join(&self, me: AccountId, username: &str, req: JoinMatch) -> Result<TurnMatch> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let id = match (req.id, &req.code) {
            (Some(id), _) => id,
            (None, Some(c)) => {
                let code = c.trim().to_ascii_uppercase();
                let r: Option<(Uuid,)> = sqlx::query_as(
                    "select id from musubi.turn_matches where code = $1 and state = 'open'",
                )
                .bind(&code)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
                r.ok_or_else(|| Error::NotFound("no open match with that code".into()))?
                    .0
            }
            (None, None) => return Err(Error::InvalidArgument("id or code is required".into())),
        };
        let mut m = self.lock(&mut tx, id).await?;
        if m.state != "open" {
            return Err(Error::Conflict(format!("match is {}", m.state)));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let joined_by_code = req.id.is_none();
        match m.players.iter_mut().find(|p| p.account_id == me) {
            Some(p) if p.status == "invited" => {
                p.status = "active".into();
                p.joined_at = now;
                p.username = username.to_string();
            }
            Some(p) if p.status == "active" => {
                return Err(Error::AlreadyExists("already in this match".into()))
            }
            Some(_) => return Err(Error::Conflict("you left this match".into())),
            None => {
                if m.visibility == "private" && !joined_by_code {
                    return Err(Error::PermissionDenied(
                        "private match: invitation or code required".into(),
                    ));
                }
                let taken = m.players.iter().filter(|p| p.status != "left").count() as i32;
                if taken >= m.max_players {
                    return Err(Error::ResourceExhausted("match is full".into()));
                }
                let seat = m.players.iter().map(|p| p.seat + 1).max().unwrap_or(0);
                m.players.push(Player {
                    account_id: me,
                    username: username.to_string(),
                    seat,
                    status: "active".into(),
                    joined_at: now,
                    skips: 0,
                    score: None,
                });
            }
        }
        m.version += 1;
        let active = m.active_seats().len() as i32;
        let started = m.auto_start && active >= m.max_players;
        if started {
            Self::start_in_place(&mut m, now);
        }
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        let others: Vec<AccountId> = Self::everyone(&m)
            .into_iter()
            .filter(|&a| a != me)
            .collect();
        self.notify_players(
            &m,
            &others,
            "joined",
            Some(me),
            json!({ "username": username }),
        )
        .await;
        if started {
            self.announce_start(&m).await;
        }
        Ok(m)
    }

    /// Creator starts an open match early (enough players, invitations still pending are dropped).
    pub async fn start(&self, me: AccountId, id: Uuid) -> Result<TurnMatch> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut m = self.lock(&mut tx, id).await?;
        if m.created_by != me {
            return Err(Error::PermissionDenied("only the creator can start".into()));
        }
        if m.state != "open" {
            return Err(Error::Conflict(format!("match is {}", m.state)));
        }
        if (m.active_seats().len() as i32) < m.min_players {
            return Err(Error::Conflict(format!(
                "need {} players, have {}",
                m.min_players,
                m.active_seats().len()
            )));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        Self::start_in_place(&mut m, now);
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        self.announce_start(&m).await;
        Ok(m)
    }

    /// Leave an open match (or decline an invitation). The creator leaving cancels it.
    pub async fn leave(&self, me: AccountId, id: Uuid) -> Result<TurnMatch> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut m = self.lock(&mut tx, id).await?;
        if m.state != "open" {
            return Err(Error::Conflict("match already started: use resign".into()));
        }
        if m.seat_of(me).is_none() {
            return Err(Error::NotFound("not in this match".into()));
        }
        let cancelled = m.created_by == me;
        if cancelled {
            m.state = "cancelled".into();
            m.finished_at = Some(OffsetDateTime::now_utc().unix_timestamp());
            m.result = json!({ "reason": "creator_left" });
        } else {
            m.players.retain(|p| p.account_id != me);
        }
        m.version += 1;
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        let others: Vec<AccountId> = Self::everyone(&m)
            .into_iter()
            .filter(|&a| a != me)
            .collect();
        self.notify_players(
            &m,
            &others,
            if cancelled { "cancelled" } else { "left" },
            Some(me),
            Value::Null,
        )
        .await;
        if cancelled {
            self.emit(
                "turn_match.cancelled",
                Some(me),
                json!({ "match_id": id, "reason": "creator_left" }),
            )
            .await;
        }
        Ok(m)
    }

    /// Finish bookkeeping shared by moves, resignations and timeouts.
    fn finish_in_place(m: &mut TurnMatch, winners: Vec<i32>, result: Value, now: i64) {
        m.state = "finished".into();
        m.winners = winners;
        m.result = result;
        m.current_seat = None;
        m.deadline_at = None;
        m.finished_at = Some(now);
    }

    /// Hand the turn on: round-robin (or `explicit`), or finish when ≤1 seat remains.
    fn advance(m: &mut TurnMatch, from: i32, explicit: Option<i32>, now: i64) -> Result<()> {
        let active = m.active_seats();
        if active.len() <= 1 {
            let result = json!({ "reason": "last_player_standing" });
            Self::finish_in_place(m, active, result, now);
            return Ok(());
        }
        let next = match explicit {
            Some(s) => {
                if !active.contains(&s) {
                    return Err(Error::InvalidArgument(format!(
                        "next_seat {s} is not an active seat"
                    )));
                }
                s
            }
            None => m.next_seat_after(from).unwrap_or(from),
        };
        m.turn += 1;
        m.current_seat = Some(next);
        m.deadline_at = Some(now + m.turn_secs);
        Ok(())
    }

    async fn announce_turn(
        &self,
        m: &TurnMatch,
        event: &str,
        actor: Option<AccountId>,
        extra: Value,
    ) {
        let all = Self::everyone(m);
        self.notify_players(m, &all, event, actor, extra).await;
        if m.state == "active" {
            if let Some(p) = m.current_player() {
                self.notify_players(m, &[p.account_id], "your_turn", None, Value::Null)
                    .await;
            }
        } else if m.state == "finished" {
            self.notify_players(
                m,
                &all,
                "finished",
                None,
                json!({ "winners": m.winners, "result": m.result }),
            )
            .await;
            self.emit(
                "turn_match.finished",
                None,
                json!({ "match_id": m.id, "game": m.game, "turns": m.turn, "winners": m.winners, "result": m.result }),
            )
            .await;
        }
    }

    pub async fn make_move(&self, me: AccountId, id: Uuid, req: MakeMove) -> Result<TurnMatch> {
        size_ok(&req.data, MOVE_BYTES, "move data")?;
        if let Some(b) = &req.board {
            size_ok(b, BOARD_BYTES, "board")?;
        }
        if let Some(f) = &req.finish {
            if f.winners.iter().any(|&w| w < 0) {
                return Err(Error::InvalidArgument("finish.winners: seats >= 0".into()));
            }
            size_ok(&f.result, METADATA_BYTES, "finish.result")?;
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut m = self.lock(&mut tx, id).await?;
        if m.state != "active" {
            return Err(Error::Conflict(format!("match is {}", m.state)));
        }
        let seat = m
            .seat_of(me)
            .filter(|p| p.active())
            .map(|p| p.seat)
            .ok_or_else(|| Error::PermissionDenied("not an active player of this match".into()))?;
        if m.current_seat != Some(seat) {
            return Err(Error::Conflict(format!(
                "not your turn (seat {:?} to move)",
                m.current_seat
            )));
        }
        if let Some(v) = req.version {
            if v != m.version {
                return Err(Error::Conflict(format!(
                    "version mismatch: match is at {}",
                    m.version
                )));
            }
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        self.record_move(&mut tx, &m, seat, Some(me), "move", &req.data)
            .await?;
        if let Some(b) = req.board {
            m.board = b;
        }
        if let Some(p) = m.players.iter_mut().find(|p| p.seat == seat) {
            p.skips = 0;
        }
        m.version += 1;
        match req.finish {
            Some(f) => {
                if f.winners
                    .iter()
                    .any(|w| !m.players.iter().any(|p| p.seat == *w))
                {
                    return Err(Error::InvalidArgument(
                        "finish.winners: unknown seat".into(),
                    ));
                }
                let result = if f.result.is_null() {
                    json!({ "reason": "declared" })
                } else {
                    f.result
                };
                Self::finish_in_place(&mut m, f.winners, result, now);
            }
            None => Self::advance(&mut m, seat, req.next_seat, now)?,
        }
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        self.announce_turn(
            &m,
            "moved",
            Some(me),
            json!({ "seat": seat, "turn": m.turn }),
        )
        .await;
        self.emit(
            "turn_match.moved",
            Some(me),
            json!({ "match_id": id, "game": m.game, "seat": seat, "turn": m.turn, "finished": m.state == "finished" }),
        )
        .await;
        Ok(m)
    }

    /// Give up an active match; the last seat standing wins.
    pub async fn resign(&self, me: AccountId, id: Uuid) -> Result<TurnMatch> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut m = self.lock(&mut tx, id).await?;
        if m.state != "active" {
            return Err(Error::Conflict(format!("match is {}", m.state)));
        }
        let seat = m
            .seat_of(me)
            .filter(|p| p.active())
            .map(|p| p.seat)
            .ok_or_else(|| Error::PermissionDenied("not an active player of this match".into()))?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        Self::resign_seat(&mut m, seat, "resigned");
        let was_current = m.current_seat == Some(seat);
        m.turn += 1;
        self.record_move(&mut tx, &m, seat, Some(me), "resign", &Value::Null)
            .await?;
        m.version += 1;
        if was_current || m.active_seats().len() <= 1 {
            // advance() bumps turn again; keep numbering contiguous
            m.turn -= 1;
            Self::advance(&mut m, seat, None, now)?;
        }
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        self.announce_turn(&m, "resigned", Some(me), json!({ "seat": seat }))
            .await;
        Ok(m)
    }

    fn resign_seat(m: &mut TurnMatch, seat: i32, status: &str) {
        if let Some(p) = m.players.iter_mut().find(|p| p.seat == seat) {
            p.status = status.into();
        }
    }

    /// New open match with the same settings; the other players are invited.
    pub async fn rematch(&self, me: AccountId, username: &str, id: Uuid) -> Result<TurnMatch> {
        let old = self.fetch(id).await?;
        if old.state != "finished" {
            return Err(Error::Conflict("rematch needs a finished match".into()));
        }
        if old.seat_of(me).is_none() {
            return Err(Error::PermissionDenied("not a player of this match".into()));
        }
        let invite: Vec<AccountId> = old
            .players
            .iter()
            .filter(|p| p.account_id != me && p.status != "left")
            .map(|p| p.account_id)
            .collect();
        let req = CreateMatch {
            game: old.game.clone(),
            min_players: Some(old.min_players),
            max_players: Some(old.max_players),
            invite,
            visibility: Some(old.visibility.clone()),
            turn_secs: Some(old.turn_secs),
            timeout_action: Some(old.timeout_action.clone()),
            max_skips: Some(old.max_skips),
            auto_start: Some(old.auto_start),
            board: Value::Null,
            metadata: old.metadata.clone(),
        };
        let m = self.create(me, username, req).await?;
        sqlx::query("update musubi.turn_matches set rematch_of = $2 where id = $1")
            .bind(m.id)
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.fetch(m.id).await
    }

    /// Server/admin: end a match by decree (dispute resolution, abuse).
    pub async fn force_finish(&self, id: Uuid, f: Finish) -> Result<TurnMatch> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut m = self.lock(&mut tx, id).await?;
        if matches!(m.state.as_str(), "finished" | "cancelled") {
            return Err(Error::Conflict(format!("match is {}", m.state)));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if m.state == "open" {
            m.state = "cancelled".into();
            m.finished_at = Some(now);
            m.result = if f.result.is_null() {
                json!({ "reason": "admin" })
            } else {
                f.result
            };
        } else {
            let result = if f.result.is_null() {
                json!({ "reason": "admin" })
            } else {
                f.result
            };
            Self::finish_in_place(&mut m, f.winners, result, now);
        }
        m.version += 1;
        self.save(&mut tx, &m).await?;
        tx.commit().await.map_err(Error::internal)?;
        let all = Self::everyone(&m);
        self.notify_players(
            &m,
            &all,
            if m.state == "finished" {
                "finished"
            } else {
                "cancelled"
            },
            None,
            json!({ "winners": m.winners, "result": m.result, "by": "server" }),
        )
        .await;
        self.emit(
            "turn_match.finished",
            None,
            json!({ "match_id": id, "game": m.game, "winners": m.winners, "result": m.result, "by": "server" }),
        )
        .await;
        Ok(m)
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let n = sqlx::query("delete from musubi.turn_matches where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("turn match {id}")));
        }
        Ok(())
    }

    /// Deadline sweep: apply `timeout_action` to every overdue turn and cancel stale open
    /// matches. Returns `(timed_out_turns, cancelled_open)`.
    pub async fn sweep(&self) -> Result<(usize, usize)> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let due: Vec<(Uuid,)> = sqlx::query_as(
            "select id from musubi.turn_matches where state = 'active' and deadline_at <= now() \
             order by deadline_at limit 500",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut timed_out = 0;
        for (id,) in due {
            let mut tx = self.db.begin().await.map_err(Error::internal)?;
            let mut m = self.lock(&mut tx, id).await?;
            if m.state != "active" || m.deadline_at.is_none_or(|d| d > now) {
                continue;
            }
            let Some(seat) = m.current_seat else { continue };
            let resign = {
                let p = m.players.iter_mut().find(|p| p.seat == seat);
                match p {
                    Some(p) => {
                        p.skips += 1;
                        m.timeout_action == "resign" || p.skips >= m.max_skips
                    }
                    None => true,
                }
            };
            let kind = if resign { "timeout" } else { "skip" };
            let account = m
                .players
                .iter()
                .find(|p| p.seat == seat)
                .map(|p| p.account_id);
            self.record_move(
                &mut tx,
                &m,
                seat,
                account,
                kind,
                &json!({ "resigned": resign }),
            )
            .await?;
            if resign {
                Self::resign_seat(&mut m, seat, "timed_out");
            }
            m.version += 1;
            Self::advance(&mut m, seat, None, now)?;
            self.save(&mut tx, &m).await?;
            tx.commit().await.map_err(Error::internal)?;
            timed_out += 1;
            self.announce_turn(
                &m,
                if resign { "timed_out" } else { "skipped" },
                None,
                json!({ "seat": seat, "resigned": resign }),
            )
            .await;
            self.emit(
                "turn_match.timed_out",
                account,
                json!({ "match_id": id, "game": m.game, "seat": seat, "resigned": resign }),
            )
            .await;
        }
        let stale: Vec<Row> = sqlx::query_as(&format!(
            "update musubi.turn_matches set state = 'cancelled', finished_at = now(), updated_at = now(), \
             result = '{{\"reason\":\"expired\"}}'::jsonb, version = version + 1 \
             where state = 'open' and created_at < now() - make_interval(secs => $1) returning {COLS}"
        ))
        .bind(OPEN_TTL as f64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let cancelled = stale.len();
        for r in stale {
            let m = TurnMatch::from(r);
            let all = Self::everyone(&m);
            self.notify_players(&m, &all, "cancelled", None, json!({ "reason": "expired" }))
                .await;
            self.emit(
                "turn_match.cancelled",
                None,
                json!({ "match_id": m.id, "reason": "expired" }),
            )
            .await;
        }
        Ok((timed_out, cancelled))
    }

    pub fn spawn_sweeper(self, active: impl Fn() -> bool + Send + 'static) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tick.tick().await;
                if !active() {
                    continue;
                }
                match self.sweep().await {
                    Ok((t, c)) if t + c > 0 => {
                        tracing::info!(timed_out = t, cancelled = c, "turn matches swept")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "turn sweeper failed"),
                }
            }
        });
    }
}
