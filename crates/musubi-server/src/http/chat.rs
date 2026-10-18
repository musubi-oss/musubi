use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_chat::{Channel, ChannelSettings, Filter, History, Moderation, Report, Unread};
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
            "/v1/channel/{id}/attachments",
            axum::routing::put(attachment_put).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/v1/channel/{id}/attachments/{attachment_id}",
            get(attachment_get),
        )
        .route("/v1/channel/unread", get(unread))
        .route("/v1/channel/{id}/read", post(mark_read))
        .route(
            "/v1/channel/{id}/messages/{message_id}/replies",
            get(replies),
        )
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
struct UnreadQuery {
    /// Comma-separated channel ids.
    channels: String,
}

async fn unread(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Query(q): Query<UnreadQuery>,
) -> Result<Json<Vec<Unread>>, ApiError> {
    let channels: Vec<String> = q
        .channels
        .split(',')
        .filter(|c| !c.is_empty())
        .take(100)
        .map(str::to_string)
        .collect();
    for c in &channels {
        s.chat.authorize(me.account_id, &Channel::parse(c)?).await?;
    }
    Ok(Json(s.chat.unread(me.account_id, &channels).await?))
}

#[derive(Deserialize, Default)]
struct ReadRequest {
    #[serde(default)]
    message_id: Option<String>,
}

async fn mark_read(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ReadRequest>>,
) -> Result<Json<Value>, ApiError> {
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    s.chat
        .mark_read(&channel, me.account_id, req.message_id.as_deref())
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn replies(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path((id, message_id)): Path<(String, String)>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    Ok(Json(
        s.chat
            .replies(&channel, &message_id, q.limit, q.cursor.as_deref())
            .await?,
    ))
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

/// Plan 5.5 attachments: raw body (`Content-Type` kept), at most `chat.attachment_max_bytes`,
/// inline in Postgres or streamed to object storage when `[ugc.s3]` is configured. The reply
/// is what a message carries in `content.attachments`; reading needs channel access.
async fn attachment_put(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, ApiError> {
    use axum::http::header;
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    s.age.check(me.account_id, "chat")?;
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or("application/octet-stream")
        .to_string();
    let max = s.cfg.chat.attachment_max_bytes.max(1);
    if let Some(len) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        if len > max {
            return Err(musubi_core::Error::InvalidArgument(format!(
                "attachment is {len} bytes, [chat] attachment_max_bytes is {max}"
            ))
            .into());
        }
    }
    let aid = Uuid::now_v7();
    let key = s
        .blobs
        .as_ref()
        .map(|_| format!("{}chat/{}/{aid}", s.cfg.ugc.s3.prefix, channel.id()));
    let streamed = match super::ugc::stream_blob(&s, body, key.as_deref(), &mime, max).await {
        Ok(v) => v,
        Err(e) => {
            if let (Some(store), Some(key), Some(upload)) = (&s.blobs, &key, e.upload.as_deref()) {
                if let Err(e) = store.abort_multipart(key, upload).await {
                    tracing::warn!(error = %e, key, "chat attachment upload not aborted");
                }
            }
            return Err(e.error.into());
        }
    };
    if streamed.size == 0 {
        if let (Some(store), Some(key)) = (&s.blobs, &key) {
            let _ = store.delete(key).await;
        }
        return Err(musubi_core::Error::InvalidArgument("empty body".into()).into());
    }
    sqlx::query(
        "insert into musubi.chat_attachments (id, channel_id, account_id, mime, size, sha256, blob, blob_key)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(aid)
    .bind(channel.id())
    .bind(me.account_id.0)
    .bind(&mime)
    .bind(streamed.size as i64)
    .bind(&streamed.sha256)
    .bind(streamed.inline.as_deref())
    .bind(&key)
    .execute(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(Json(json!({
        "id": aid,
        "url": format!("/v1/channel/{}/attachments/{aid}", channel.id()),
        "mime": mime,
        "size": streamed.size,
        "sha256": streamed.sha256,
    })))
}

type AttachmentRow = (String, i64, Option<Vec<u8>>, Option<String>);

async fn attachment_get(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, attachment_id)): Path<(String, Uuid)>,
) -> Result<axum::response::Response, ApiError> {
    use axum::http::header;
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    let row: Option<AttachmentRow> = sqlx::query_as(
        "select mime, size, blob, blob_key from musubi.chat_attachments where id = $1 and channel_id = $2",
    )
    .bind(attachment_id)
    .bind(channel.id())
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let Some((mime, size, blob, key)) = row else {
        return Err(musubi_core::Error::NotFound(format!("attachment {attachment_id}")).into());
    };
    let body = match (blob, key, &s.blobs) {
        (Some(b), _, _) => axum::body::Body::from(b),
        (None, Some(key), Some(store)) => {
            let (_, stream) = store.get_stream(&key).await?;
            axum::body::Body::from_stream(stream)
        }
        _ => {
            return Err(musubi_core::Error::Unavailable(
                "attachment is in object storage but [ugc.s3] is not configured on this node"
                    .into(),
            )
            .into())
        }
    };
    let res = axum::response::Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, size)
        .header(header::CACHE_CONTROL, "private, max-age=86400")
        .body(body)
        .map_err(musubi_core::Error::internal)?;
    Ok(res)
}
