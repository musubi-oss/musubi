//! Game sessions (P1-16/56).
//!
//! Server: `POST /v1/server/sessions` (allocate a dedicated server / launch a match / record custom
//! connect info, idempotent by `idempotency_key`), `POST …/search`, `GET|PATCH|DELETE …/{id}`,
//! `GET …/{id}/members`, `POST …/{id}/members {account_id}` (reserve a seat for a player),
//! `DELETE …/{id}/members/{account_id}`, templates `GET|PUT|DELETE /v1/server/session_templates[/{id}]`.
//! Game server SDK: `GET|PATCH /v1/gameserver/session`, `POST /v1/gameserver/session/accept {token}`,
//! `POST /v1/gameserver/session/left {account_id}`.
//! Player: `POST /v1/sessions {template_id, …}` (private/custom match), `POST /v1/sessions/search`
//! (server browser), `POST /v1/sessions/join {code}`, `GET /v1/sessions/mine|history`,
//! `GET|PATCH|DELETE /v1/sessions/{id}`, `POST …/{id}/join|leave|invite|code`.

use crate::app::AppState;
use crate::game_sessions::{
    normalize_code, parse_id, NewSession, SearchSessions, Session, Template, UpdateSession,
};
use crate::http::extract::{Bearer, GameServerAuth, ServerKey};
use crate::http::ApiError;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_social::NewNotification;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

const SESSION_INVITE_CODE: i32 = 32;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/sessions", get(list_server).post(create))
        .route("/v1/server/sessions/search", post(search_server))
        .route("/v1/server/sessions/stop", post(stop_many))
        .route(
            "/v1/server/sessions/{id}",
            get(get_server)
                .patch(update_server)
                .delete(terminate_server),
        )
        .route(
            "/v1/server/sessions/{id}/members",
            get(members).post(reserve_for),
        )
        .route(
            "/v1/server/sessions/{id}/members/{account_id}",
            delete(remove_member),
        )
        .route("/v1/server/session_templates", get(templates))
        .route(
            "/v1/server/session_templates/{id}",
            put(put_template).delete(delete_template),
        )
        .route("/v1/gameserver/session", get(sdk_session).patch(sdk_update))
        .route("/v1/gameserver/session/accept", post(sdk_accept))
        .route("/v1/gameserver/session/left", post(sdk_left))
        .route("/v1/sessions", post(create_mine))
        .route("/v1/sessions/search", post(search_public))
        .route("/v1/sessions/join", post(join_by_code))
        .route("/v1/sessions/mine", get(mine))
        .route("/v1/sessions/history", get(history))
        .route(
            "/v1/sessions/{id}",
            get(get_mine).patch(update_mine).delete(terminate_mine),
        )
        .route("/v1/sessions/{id}/join", post(join))
        .route("/v1/sessions/{id}/leave", post(leave))
        .route("/v1/sessions/{id}/invite", post(invite))
        .route("/v1/sessions/{id}/code", post(rotate_code))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateReq {
    name: String,
    /// Dedicated server from this fleet …
    fleet_id: Option<String>,
    /// … or from a pool/fleet by name (region-routed) …
    pool: Option<String>,
    region: Option<String>,
    backfill: bool,
    wait_secs: u64,
    /// … or an authoritative match of this module …
    module: Option<String>,
    params: Value,
    /// … or custom connect info the game manages itself.
    connect: Option<Value>,
    properties: Value,
    data: Value,
    max_players: i32,
    join_policy: Option<String>,
    idempotency_key: Option<String>,
    owner_id: Option<AccountId>,
}

struct Backend {
    kind: &'static str,
    fleet_id: Option<Uuid>,
    server_id: Option<Uuid>,
    match_id: Option<String>,
    region: String,
    connect: Value,
}

/// Allocate/launch whatever the request asks for; `payload` is what the game server / match sees.
async fn start_backend(
    s: &AppState,
    id: Uuid,
    r: &CreateReq,
    mut payload: Value,
) -> Result<Backend, ApiError> {
    if !payload.is_object() {
        payload = json!({});
    }
    payload["session_id"] = json!(id);
    payload["properties"] = r.properties.clone();
    if let Some(o) = r.owner_id {
        payload["owner_id"] = json!(o);
    }
    let wait = std::time::Duration::from_secs(r.wait_secs.min(300));
    if let Some(fid) = r.fleet_id.as_deref() {
        let fleet_id: Uuid = fid
            .parse()
            .map_err(|_| Error::InvalidArgument("bad fleet id".into()))?;
        let a = if r.wait_secs > 0 {
            let f = s.fleets.get(fleet_id).await?;
            s.fleets
                .allocate_routed(&f.name, r.region.as_deref(), payload, r.backfill, wait)
                .await?
        } else {
            s.fleets.allocate(fleet_id, payload, r.backfill).await?
        };
        let region = s
            .fleets
            .get(a.fleet_id)
            .await
            .map(|f| f.region)
            .unwrap_or_default();
        return Ok(Backend {
            kind: "dedicated",
            fleet_id: Some(a.fleet_id),
            server_id: Some(a.server_id),
            match_id: None,
            region,
            connect: json!({ "address": a.address, "port": a.port, "connect_token": a.connect_token, "backfill": a.backfill }),
        });
    }
    if let Some(pool) = r.pool.as_deref() {
        let a = s
            .fleets
            .allocate_routed(pool, r.region.as_deref(), payload, r.backfill, wait)
            .await?;
        let region = s
            .fleets
            .get(a.fleet_id)
            .await
            .map(|f| f.region)
            .unwrap_or_default();
        return Ok(Backend {
            kind: "dedicated",
            fleet_id: Some(a.fleet_id),
            server_id: Some(a.server_id),
            match_id: None,
            region,
            connect: json!({ "address": a.address, "port": a.port, "connect_token": a.connect_token, "backfill": a.backfill }),
        });
    }
    if let Some(module) = r.module.as_deref() {
        s.idl.check_match_params(module, &payload)?;
        let h = s.matches.create(s.hub.clone(), module, payload)?;
        let info = h.info().await;
        return Ok(Backend {
            kind: "match",
            fleet_id: None,
            server_id: None,
            match_id: Some(info.match_id.clone()),
            region: info.region.clone(),
            connect: json!({ "match_id": info.match_id, "module": info.module, "label": info.label }),
        });
    }
    if let Some(c) = &r.connect {
        return Ok(Backend {
            kind: "custom",
            fleet_id: None,
            server_id: None,
            match_id: None,
            region: r.region.clone().unwrap_or_default(),
            connect: c.clone(),
        });
    }
    Err(Error::InvalidArgument("fleet_id, pool, module or connect is required".into()).into())
}

async fn stop_backend(s: &AppState, sess: &Session) {
    match sess.kind.as_str() {
        "match" => {
            if let Some(h) = sess.match_id.as_deref().and_then(|m| s.matches.get(m)) {
                h.terminate(0);
            }
        }
        "dedicated" => {
            if let Some(sid) = sess.server_id {
                if let Err(e) = s.fleets.sdk_shutdown(sid).await {
                    tracing::debug!(error = %e, server = %sid, "session terminate: server already gone");
                }
            }
        }
        _ => {}
    }
}

async fn create_session(s: &AppState, r: CreateReq) -> Result<(Session, bool), ApiError> {
    if let Some(k) = r.idempotency_key.as_deref() {
        if k.is_empty() || k.len() > 128 {
            return Err(Error::InvalidArgument("idempotency_key: 1..128 chars".into()).into());
        }
        if let Some(existing) = s.game_sessions.find_idempotent(k).await? {
            return Ok((existing, true));
        }
    }
    crate::lobby::validate_attrs(&r.properties, "properties")?;
    let id = Uuid::now_v7();
    let b = start_backend(s, id, &r, r.params.clone()).await?;
    let created = s
        .game_sessions
        .insert(NewSession {
            id,
            name: r.name.clone(),
            kind: b.kind.to_string(),
            owner_id: r.owner_id,
            template_id: None,
            fleet_id: b.fleet_id,
            server_id: b.server_id,
            match_id: b.match_id.clone(),
            region: b.region.clone(),
            connect: b.connect.clone(),
            properties: r.properties.clone(),
            data: r.data.clone(),
            max_players: r.max_players,
            join_policy: r.join_policy.clone().unwrap_or_else(|| "open".into()),
            idempotency_key: r.idempotency_key.clone(),
        })
        .await;
    let created = match created {
        Ok(c) => c,
        Err(e) => {
            // the backend was started for nothing: release it
            let ghost = Session {
                id,
                name: String::new(),
                kind: b.kind.to_string(),
                state: "terminated".into(),
                owner_id: None,
                template_id: None,
                fleet_id: None,
                server_id: b.server_id,
                match_id: b.match_id.clone(),
                region: String::new(),
                connect: None,
                properties: json!({}),
                data: json!({}),
                max_players: 0,
                players: 0,
                free_slots: None,
                join_policy: "open".into(),
                join_code: None,
                idempotency_key: None,
                created_at: 0,
                updated_at: 0,
                terminated_at: None,
            };
            stop_backend(s, &ghost).await;
            return Err(e.into());
        }
    };
    let reused = created.id != id;
    if !reused {
        s.events
            .emit(
                "session.created",
                created.owner_id,
                json!({ "session_id": created.id, "kind": created.kind, "region": created.region, "join_policy": created.join_policy }),
            )
            .await;
    }
    Ok((created, reused))
}

fn with_reused(sess: Session, reused: bool) -> Value {
    let mut v = serde_json::to_value(sess).unwrap_or_default();
    v["reused"] = json!(reused);
    v
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateReq>,
) -> Result<Json<Value>, ApiError> {
    let (sess, reused) = create_session(&s, r).await?;
    Ok(Json(with_reused(sess, reused)))
}

/// `GET /v1/server/sessions?kind=&region=&has_slots=&include_terminated=&limit=&offset=` — the
/// same search as `POST /search` for callers that can only GET (console lists, curl).
async fn list_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(r): Query<SearchSessions>,
) -> Result<Json<Value>, ApiError> {
    let page = s.game_sessions.search(&r, false).await?;
    Ok(Json(serde_json::to_value(page).unwrap_or_default()))
}

async fn search_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<SearchSessions>,
) -> Result<Json<Value>, ApiError> {
    let page = s.game_sessions.search(&r, false).await?;
    Ok(Json(serde_json::to_value(page).unwrap_or_default()))
}

async fn get_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Session>, ApiError> {
    Ok(Json(s.game_sessions.get(parse_id(&id)?).await?))
}

async fn update_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(u): Json<UpdateSession>,
) -> Result<Json<Session>, ApiError> {
    let sess = s.game_sessions.update(parse_id(&id)?, u).await?;
    s.events
        .emit("session.updated", None, json!({ "session_id": sess.id }))
        .await;
    Ok(Json(sess))
}

async fn do_terminate(s: &AppState, id: Uuid, by: Option<AccountId>) -> Result<Value, ApiError> {
    match s.game_sessions.terminate(id).await? {
        Some(before) => {
            stop_backend(s, &before).await;
            s.events
                .emit(
                    "session.terminated",
                    by,
                    json!({ "session_id": id, "kind": before.kind, "players": before.players }),
                )
                .await;
            Ok(json!({ "ok": true, "terminated": true }))
        }
        None => Ok(json!({ "ok": true, "terminated": false })),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StopMany {
    ids: Vec<String>,
    /// Alternative to `ids`: every live session matching this server search (≤ 500 per call).
    search: Option<crate::game_sessions::SearchSessions>,
    reason: Option<String>,
    dry_run: bool,
}

/// Bulk stop: by explicit ids or by search (same body as `/v1/server/sessions/search`) — empty a
/// region before maintenance, kill every session of a broken build, clear a test namespace.
async fn stop_many(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<StopMany>,
) -> Result<Json<Value>, ApiError> {
    if r.ids.is_empty() == r.search.is_none() {
        return Err(
            musubi_core::Error::InvalidArgument("stop: give either ids or search".into()).into(),
        );
    }
    if r.ids.len() > 500 {
        return Err(musubi_core::Error::InvalidArgument("stop: at most 500 ids".into()).into());
    }
    let ids: Vec<Uuid> = if let Some(mut q) = r.search {
        q.include_terminated = false;
        q.limit = Some(q.limit.unwrap_or(500).clamp(1, 500));
        q.offset = None;
        s.game_sessions
            .search(&q, false)
            .await?
            .sessions
            .into_iter()
            .map(|x| x.id)
            .collect()
    } else {
        r.ids
            .iter()
            .map(|i| parse_id(i))
            .collect::<Result<_, _>>()?
    };
    if r.dry_run {
        return Ok(Json(
            json!({ "dry_run": true, "matched": ids.len(), "ids": ids }),
        ));
    }
    let mut stopped = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match s.game_sessions.terminate(id).await {
            Ok(Some(before)) => {
                stop_backend(&s, &before).await;
                s.events
                    .emit(
                        "session.terminated",
                        None,
                        json!({ "id": id, "kind": before.kind, "bulk": true, "reason": r.reason }),
                    )
                    .await;
                stopped.push(id);
            }
            Ok(None) | Err(musubi_core::Error::NotFound(_)) => missing.push(id),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Json(
        json!({ "stopped": stopped.len(), "ids": stopped, "missing": missing, "reason": r.reason }),
    ))
}

async fn terminate_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(do_terminate(&s, parse_id(&id)?, None).await?))
}

async fn members(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    s.game_sessions.get(id).await?;
    Ok(Json(
        json!({ "members": s.game_sessions.members(id).await? }),
    ))
}

#[derive(Deserialize)]
struct ReserveReq {
    account_id: AccountId,
}

/// Server-side seat reservation (GameLift `CreatePlayerSession`): bypasses the join policy.
async fn reserve_for(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<ReserveReq>,
) -> Result<Json<Value>, ApiError> {
    let (sess, m) = s
        .game_sessions
        .reserve(parse_id(&id)?, r.account_id, None, true)
        .await?;
    s.events
        .emit(
            "session.player_reserved",
            Some(r.account_id),
            json!({ "session_id": sess.id, "by": "server" }),
        )
        .await;
    Ok(Json(json!({ "session": sess, "member": m })))
}

async fn remove_member(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, account_id)): Path<(String, AccountId)>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    let removed = s.game_sessions.remove_member(id, account_id).await?;
    if removed {
        s.events
            .emit(
                "session.player_left",
                Some(account_id),
                json!({ "session_id": id, "by": "server", "removed": true }),
            )
            .await;
    }
    Ok(Json(json!({ "removed": removed })))
}

async fn templates(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "templates": s.game_sessions.templates().await? }),
    ))
}

async fn put_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(t): Json<Template>,
) -> Result<Json<Template>, ApiError> {
    Ok(Json(s.game_sessions.put_template(&id, t).await?))
}

async fn delete_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "deleted": s.game_sessions.delete_template(&id).await? }),
    ))
}

// ----- game server SDK -----

async fn sdk_session(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.by_server(sid).await?;
    let members = s.game_sessions.members(sess.id).await?;
    Ok(Json(json!({ "session": sess, "members": members })))
}

async fn sdk_update(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
    Json(u): Json<UpdateSession>,
) -> Result<Json<Session>, ApiError> {
    let sess = s.game_sessions.by_server(sid).await?;
    let sess = s.game_sessions.update(sess.id, u).await?;
    s.events
        .emit(
            "session.updated",
            None,
            json!({ "session_id": sess.id, "by": "gameserver" }),
        )
        .await;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct AcceptReq {
    token: String,
}

/// The game server confirms a connecting player's seat token → `joined`. Unknown, expired or
/// `left` tokens are 401 so the server can refuse the connection.
async fn sdk_accept(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
    Json(r): Json<AcceptReq>,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.by_server(sid).await?;
    let m = s.game_sessions.accept(sess.id, &r.token).await?;
    s.events
        .emit(
            "session.player_joined",
            Some(m.account_id),
            json!({ "session_id": sess.id }),
        )
        .await;
    Ok(Json(
        json!({ "account_id": m.account_id, "session_id": sess.id, "member": m }),
    ))
}

#[derive(Deserialize)]
struct LeftReq {
    account_id: AccountId,
}

async fn sdk_left(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
    Json(r): Json<LeftReq>,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.by_server(sid).await?;
    let left = s.game_sessions.leave(sess.id, r.account_id).await?;
    if left {
        s.events
            .emit(
                "session.player_left",
                Some(r.account_id),
                json!({ "session_id": sess.id, "by": "gameserver" }),
            )
            .await;
    }
    Ok(Json(json!({ "left": left })))
}

// ----- players -----

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateMineReq {
    template_id: String,
    name: String,
    properties: Value,
    data: Value,
    /// Override the template's policy (`open` makes it a public custom lobby).
    join_policy: Option<String>,
    /// Smaller than the template's cap is allowed, larger is not.
    max_players: Option<i32>,
}

/// Player-initiated private/custom match from an operator template: allocates the backend,
/// reserves the owner's seat and returns the seat token + connect info + invite code.
async fn create_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<CreateMineReq>,
) -> Result<Json<Value>, ApiError> {
    let t = s.game_sessions.template(&r.template_id).await?;
    if !t.enabled {
        return Err(Error::PermissionDenied("session template is disabled".into()).into());
    }
    let active = s.game_sessions.active_owned(me.account_id, &t.id).await?;
    if active >= t.max_active_per_owner as i64 {
        return Err(Error::ResourceExhausted(format!(
            "you already have {active} active session(s) of this template"
        ))
        .into());
    }
    let max_players = match r.max_players {
        Some(m) if m < 1 || m > t.max_players => {
            return Err(
                Error::InvalidArgument(format!("max_players: 1..={}", t.max_players)).into(),
            )
        }
        Some(m) => m,
        None => t.max_players,
    };
    let req = CreateReq {
        name: r.name.clone(),
        pool: t.fleet.clone(),
        region: t.region.clone(),
        wait_secs: t.wait_secs.max(0) as u64,
        module: t.module.clone(),
        params: t.params.clone(),
        properties: r.properties.clone(),
        data: r.data.clone(),
        max_players,
        join_policy: Some(
            r.join_policy
                .clone()
                .unwrap_or_else(|| t.join_policy.clone()),
        ),
        owner_id: Some(me.account_id),
        ..Default::default()
    };
    let (sess, _) = create_session(&s, req).await?;
    sqlx::query("update musubi.game_sessions set template_id = $2 where id = $1")
        .bind(sess.id)
        .bind(&t.id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    let (sess, m) = s
        .game_sessions
        .reserve(sess.id, me.account_id, None, true)
        .await?;
    Ok(Json(json!({ "session": sess, "member": m })))
}

async fn search_public(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Json(r): Json<SearchSessions>,
) -> Result<Json<Value>, ApiError> {
    let page = s.game_sessions.search(&r, true).await?;
    Ok(Json(serde_json::to_value(page).unwrap_or_default()))
}

#[derive(Deserialize)]
struct JoinCodeReq {
    code: String,
}

async fn reserve_me(
    s: &AppState,
    id: Uuid,
    me: AccountId,
    code: Option<&str>,
) -> Result<Value, ApiError> {
    let (sess, m) = s.game_sessions.reserve(id, me, code, false).await?;
    s.events
        .emit(
            "session.player_reserved",
            Some(me),
            json!({ "session_id": sess.id, "by": "player" }),
        )
        .await;
    Ok(json!({ "session": sess, "member": m }))
}

async fn join_by_code(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<JoinCodeReq>,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.by_code(&r.code).await?;
    Ok(Json(
        reserve_me(&s, sess.id, me.account_id, Some(&normalize_code(&r.code))).await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JoinReq {
    code: Option<String>,
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    body: Option<Json<JoinReq>>,
) -> Result<Json<Value>, ApiError> {
    let code = body.and_then(|Json(b)| b.code);
    Ok(Json(
        reserve_me(&s, parse_id(&id)?, me.account_id, code.as_deref()).await?,
    ))
}

async fn leave(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id)?;
    let left = s.game_sessions.leave(id, me.account_id).await?;
    if left {
        s.events
            .emit(
                "session.player_left",
                Some(me.account_id),
                json!({ "session_id": id, "by": "player" }),
            )
            .await;
    }
    Ok(Json(json!({ "left": left })))
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "sessions": s.game_sessions.mine(me.account_id).await? }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HistoryQuery {
    limit: Option<i64>,
}

async fn history(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "sessions": s.game_sessions.history(me.account_id, q.limit.unwrap_or(50)).await?
    })))
}

/// Members and the owner see connect info and the code; everyone else the public card.
async fn get_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.get(parse_id(&id)?).await?;
    let insider = s.game_sessions.is_insider(&sess, me.account_id).await?;
    let member = s.game_sessions.member(sess.id, me.account_id).await?;
    let sess = if insider { sess } else { sess.public() };
    Ok(Json(json!({ "session": sess, "member": member })))
}

async fn owned(s: &AppState, id: &str, me: AccountId) -> Result<Session, ApiError> {
    let sess = s.game_sessions.get(parse_id(id)?).await?;
    if sess.owner_id != Some(me) {
        return Err(Error::PermissionDenied("only the session owner can do this".into()).into());
    }
    Ok(sess)
}

async fn update_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(u): Json<UpdateSession>,
) -> Result<Json<Session>, ApiError> {
    let sess = owned(&s, &id, me.account_id).await?;
    if let Some(m) = u.max_players {
        let cap = match sess.template_id.as_deref() {
            Some(t) => s
                .game_sessions
                .template(t)
                .await
                .map(|t| t.max_players)
                .unwrap_or(sess.max_players),
            None => sess.max_players,
        };
        if m < 1 || m > cap {
            return Err(Error::InvalidArgument(format!("max_players: 1..={cap}")).into());
        }
    }
    let sess = s.game_sessions.update(sess.id, u).await?;
    s.events
        .emit(
            "session.updated",
            Some(me.account_id),
            json!({ "session_id": sess.id }),
        )
        .await;
    Ok(Json(sess))
}

async fn terminate_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sess = owned(&s, &id, me.account_id).await?;
    Ok(Json(do_terminate(&s, sess.id, Some(me.account_id)).await?))
}

#[derive(Deserialize)]
struct InviteReq {
    account_id: AccountId,
}

/// Owner or a live member invites a friend: an `invited` seat (bypasses the policy) plus a
/// `session_invite` notification carrying the id and code.
async fn invite(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(r): Json<InviteReq>,
) -> Result<Json<Value>, ApiError> {
    let sess = s.game_sessions.get(parse_id(&id)?).await?;
    if !s.game_sessions.is_insider(&sess, me.account_id).await? {
        return Err(Error::PermissionDenied("only members can invite".into()).into());
    }
    if r.account_id == me.account_id {
        return Err(Error::InvalidArgument("cannot invite yourself".into()).into());
    }
    let m = s.game_sessions.invite(sess.id, r.account_id).await?;
    let _ = s
        .notify
        .send(vec![NewNotification {
            user_id: r.account_id,
            sender_id: Some(me.account_id),
            subject: "session_invite".into(),
            content: json!({ "session_id": sess.id, "name": sess.name, "code": sess.join_code, "from": me.account_id }),
            code: SESSION_INVITE_CODE,
            persistent: true,
            ttl_secs: Some(86_400),
        }])
        .await;
    s.events
        .emit(
            "session.invited",
            Some(me.account_id),
            json!({ "session_id": sess.id, "account_id": r.account_id }),
        )
        .await;
    Ok(Json(json!({ "member": m, "code": sess.join_code })))
}

async fn rotate_code(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sess = owned(&s, &id, me.account_id).await?;
    let sess = s.game_sessions.rotate_code(sess.id).await?;
    Ok(Json(json!({ "code": sess.join_code })))
}
