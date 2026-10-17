use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_chat::{Channel, ChannelSettings, Filter, History, Moderation, Report};
use musubi_core::AccountId;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/channel/{id}/messages", get(history))
        .route(
            "/v1/channel/{id}/messages/{message_id}/reactions",
            post(react),
        )
        .route(
            "/v1/channel/{id}/messages/{message_id}/reactions/{emoji}",
            delete(unreact),
        )
        .route(
            "/v1/admin/chat/{id}/moderation",
            get(list_moderation).post(moderate),
        )
        .route(
            "/v1/admin/chat/{id}/moderation/{account_id}",
            delete(unmoderate),
        )
        .route(
            "/v1/admin/chat/{id}/messages/{message_id}",
            delete(delete_message),
        )
        .route("/v1/channel/{id}/report", post(report))
        .route(
            "/v1/admin/chat/filters",
            get(list_filters).put(upsert_filter),
        )
        .route("/v1/admin/chat/filters/{filter_id}", delete(delete_filter))
        .route(
            "/v1/admin/chat/{id}/settings",
            get(get_settings).put(set_settings),
        )
        .route("/v1/admin/chat/reports", get(list_reports))
        .route(
            "/v1/admin/chat/reports/{report_id}",
            get(get_report).post(resolve_report),
        )
}

#[derive(Deserialize)]
struct ReportRequest {
    account_id: Uuid,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    reason: String,
}

/// Report a participant of a channel; the last 20 messages are attached as context for moderators.
async fn report(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ReportRequest>,
) -> Result<Json<Report>, ApiError> {
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    Ok(Json(
        s.chat
            .report(
                &channel,
                me.account_id,
                AccountId(req.account_id),
                req.message_id.as_deref(),
                &req.reason,
            )
            .await?,
    ))
}

async fn list_filters(
    _k: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Vec<Filter>>, ApiError> {
    Ok(Json(s.chat.filters().await?))
}

async fn upsert_filter(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(f): Json<Filter>,
) -> Result<Json<Filter>, ApiError> {
    Ok(Json(s.chat.upsert_filter(f).await?))
}

async fn delete_filter(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(filter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.chat.delete_filter(&filter_id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SettingsRequest {
    #[serde(default)]
    slow_mode_secs: i64,
}

async fn get_settings(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChannelSettings>, ApiError> {
    let channel = Channel::parse(&id)?;
    Ok(Json(s.chat.channel_settings(&channel.id()).await?))
}

async fn set_settings(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SettingsRequest>,
) -> Result<Json<ChannelSettings>, ApiError> {
    let channel = Channel::parse(&id)?;
    Ok(Json(
        s.chat
            .set_channel_settings(&channel.id(), req.slow_mode_secs)
            .await?,
    ))
}

#[derive(Deserialize)]
struct ReportsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

async fn list_reports(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ReportsQuery>,
) -> Result<Json<Vec<Report>>, ApiError> {
    Ok(Json(s.chat.reports(q.status.as_deref(), q.limit).await?))
}

async fn get_report(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(report_id): Path<String>,
) -> Result<Json<Report>, ApiError> {
    Ok(Json(s.chat.report_get(&report_id).await?))
}

#[derive(Deserialize)]
struct ResolveRequest {
    /// `resolved`, `dismissed` or `open` (reopen).
    status: String,
    #[serde(default)]
    by: Option<String>,
}

async fn resolve_report(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(report_id): Path<String>,
    Json(req): Json<ResolveRequest>,
) -> Result<Json<Report>, ApiError> {
    Ok(Json(
        s.chat
            .resolve_report(&report_id, &req.status, req.by.as_deref().or(Some("admin")))
            .await?,
    ))
}

#[derive(Deserialize)]
struct ModerateRequest {
    account_id: Uuid,
    /// `mute` (cannot send), `ban` (cannot join or send) or `shadow` (sends go only to the sender).
    kind: String,
    #[serde(default)]
    reason: Option<String>,
    /// Duration in seconds; omit for indefinite.
    #[serde(default)]
    duration_secs: Option<u64>,
}

async fn list_moderation(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Moderation>>, ApiError> {
    let channel = Channel::parse(&id)?;
    Ok(Json(s.chat.moderations(&channel).await?))
}

async fn moderate(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ModerateRequest>,
) -> Result<Json<Value>, ApiError> {
    let channel = Channel::parse(&id)?;
    let account = AccountId(req.account_id);
    s.chat
        .moderate(
            &channel,
            account,
            &req.kind,
            req.reason.as_deref(),
            Some("admin"),
            req.duration_secs,
        )
        .await?;
    if req.kind == "ban" {
        s.fanout.leave_channel_everywhere(&channel.id(), account);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn unmoderate(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((id, account_id)): Path<(String, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let channel = Channel::parse(&id)?;
    let removed = s.chat.unmoderate(&channel, AccountId(account_id)).await?;
    Ok(Json(json!({ "removed": removed })))
}

async fn delete_message(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((id, message_id)): Path<(String, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let channel = Channel::parse(&id)?;
    let removed = s.chat.remove_as_moderator(&channel, message_id).await?;
    Ok(Json(json!({ "removed": removed })))
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    forward: bool,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_limit() -> usize {
    50
}

async fn history(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    Ok(Json(
        s.chat
            .history(
                &channel,
                q.limit,
                q.forward,
                q.cursor.as_deref(),
                Some(me.account_id),
            )
            .await?,
    ))
}

#[derive(Deserialize)]
struct ReactReq {
    emoji: String,
}

async fn react(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, message_id)): Path<(String, String)>,
    Json(b): Json<ReactReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    toggle_reaction(&s, me.account_id, &id, &message_id, &b.emoji, true).await
}

async fn unreact(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, message_id, emoji)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    toggle_reaction(&s, me.account_id, &id, &message_id, &emoji, false).await
}

async fn toggle_reaction(
    s: &AppState,
    account_id: musubi_core::AccountId,
    id: &str,
    message_id: &str,
    emoji: &str,
    add: bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    let channel = Channel::parse(id)?;
    s.chat.authorize(account_id, &channel).await?;
    let count = s
        .chat
        .react(&channel, account_id, message_id, emoji, add)
        .await?;
    let username = s
        .accounts
        .get_users(&[account_id], &[])
        .await?
        .into_iter()
        .next()
        .map(|u| u.username)
        .unwrap_or_default();
    s.fanout.channel(
        id,
        musubi_proto::realtime::ServerMessage::ChannelMessageReaction {
            channel_id: id.to_string(),
            message_id: message_id.to_string(),
            emoji: emoji.to_string(),
            account_id,
            username,
            added: add,
            count,
        },
    );
    Ok(Json(json!({ "emoji": emoji, "count": count, "me": add })))
}
