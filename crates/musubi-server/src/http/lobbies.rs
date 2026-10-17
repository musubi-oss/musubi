//! Lobby API (plan 5.4); see `crate::lobby`.

use crate::app::AppState;
use crate::http::{ApiError, Bearer};
use crate::lobby::{CreateLobby, JoinLobby, ListQuery, Lobby, LobbyPage, UpdateLobby};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/lobbies", get(list).post(create))
        .route("/v1/lobbies/join", post(join))
        .route("/v1/lobbies/mine", get(mine))
        .route("/v1/lobbies/{id}", get(get_one).patch(update).delete(close))
        .route("/v1/lobbies/{id}/leave", post(leave))
        .route("/v1/lobbies/{id}/kick", post(kick))
        .route("/v1/lobbies/{id}/ready", post(ready))
        .route("/v1/lobbies/{id}/team", post(team))
        .route("/v1/lobbies/{id}/heartbeat", post(heartbeat))
        .route("/v1/lobbies/{id}/start", post(start))
}

async fn username(s: &AppState, me: &musubi_auth::Authenticated) -> Result<String, ApiError> {
    match &me.username {
        Some(u) => Ok(u.clone()),
        None => Ok(s.accounts.get(me.account_id).await?.username),
    }
}

async fn list(
    Bearer(_me): Bearer,
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<LobbyPage>, ApiError> {
    Ok(Json(s.lobbies.list(&q).await?))
}

async fn create(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(req): Json<CreateLobby>,
) -> Result<Json<Lobby>, ApiError> {
    let name = username(&s, &me).await?;
    Ok(Json(s.lobbies.create(me.account_id, &name, req).await?))
}

async fn join(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(req): Json<JoinLobby>,
) -> Result<Json<Lobby>, ApiError> {
    let name = username(&s, &me).await?;
    Ok(Json(s.lobbies.join(me.account_id, &name, req).await?))
}

async fn mine(Bearer(me): Bearer, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "lobby": s.lobbies.mine(me.account_id).await? }),
    ))
}

async fn get_one(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Lobby>, ApiError> {
    Ok(Json(s.lobbies.get(&id, Some(me.account_id)).await?))
}

async fn update(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateLobby>,
) -> Result<Json<Lobby>, ApiError> {
    Ok(Json(s.lobbies.update(me.account_id, &id, req).await?))
}

async fn close(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.lobbies.close(me.account_id, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn leave(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.lobbies.leave(me.account_id, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct KickReq {
    account_id: AccountId,
}

async fn kick(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<KickReq>,
) -> Result<Json<Value>, ApiError> {
    s.lobbies.kick(me.account_id, &id, r.account_id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ReadyReq {
    ready: bool,
}

async fn ready(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<ReadyReq>,
) -> Result<Json<Lobby>, ApiError> {
    Ok(Json(
        s.lobbies.set_ready(me.account_id, &id, r.ready).await?,
    ))
}

#[derive(Deserialize)]
struct TeamReq {
    team: i32,
}

async fn team(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<TeamReq>,
) -> Result<Json<Lobby>, ApiError> {
    Ok(Json(s.lobbies.set_team(me.account_id, &id, r.team).await?))
}

async fn heartbeat(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.lobbies.heartbeat(me.account_id, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// How the lobby turns into a game: a dedicated server from a fleet, an authoritative match
/// on this cluster, or connect info the host supplies itself (listen server / P2P).
#[derive(Deserialize, Default)]
#[serde(default)]
struct StartReq {
    fleet_id: Option<String>,
    payload: Value,
    module: Option<String>,
    params: Value,
    connect: Option<Value>,
    require_ready: bool,
}

async fn start(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<StartReq>,
) -> Result<Json<Lobby>, ApiError> {
    let lobby = s
        .lobbies
        .check_start(me.account_id, &id, r.require_ready)
        .await?;
    let roster: Vec<Value> = lobby
        .members
        .iter()
        .map(|m| json!({ "account_id": m.account_id, "username": m.username, "team": m.team }))
        .collect();
    let started = if let Some(fleet) = r.fleet_id.as_deref() {
        let fleet_id: uuid::Uuid = fleet
            .parse()
            .map_err(|_| Error::InvalidArgument("bad fleet id".into()))?;
        let mut payload = r.payload;
        if payload.is_null() {
            payload = json!({});
        }
        payload["lobby_id"] = json!(lobby.id);
        payload["members"] = json!(roster);
        let a = s.fleets.allocate(fleet_id, payload, false).await?;
        json!({
            "kind": "dedicated",
            "server_id": a.server_id,
            "fleet_id": a.fleet_id,
            "address": a.address,
            "port": a.port,
            "connect_token": a.connect_token,
        })
    } else if let Some(module) = r.module.as_deref() {
        let mut params = r.params;
        if params.is_null() {
            params = json!({});
        }
        params["lobby_id"] = json!(lobby.id);
        params["members"] = json!(roster);
        let h = s.matches.create(s.hub.clone(), module, params)?;
        let info = h.info().await;
        json!({ "kind": "match", "match_id": info.match_id, "module": info.module, "label": info.label })
    } else if let Some(c) = r.connect {
        json!({ "kind": "custom", "connect": c })
    } else {
        return Err(Error::InvalidArgument("fleet_id, module or connect required".into()).into());
    };
    let lobby = s.lobbies.mark_started(me.account_id, &id, started).await?;
    s.events
        .emit(
            "lobby.started",
            Some(me.account_id),
            json!({ "lobby_id": lobby.id, "members": roster.len(), "started": lobby.started }),
        )
        .await;
    Ok(Json(lobby))
}
