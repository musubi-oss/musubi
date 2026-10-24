use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_social::notifications::code;
use musubi_social::{FriendList, FriendState, NewNotification};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/friends", get(list).post(add).delete(remove))
        .route("/v1/friends/block", post(block))
        .route("/v1/friends/recent", get(recent))
        .route("/v1/friends/suggestions", get(suggestions))
        .route("/v1/friends/import", post(import).get(import_list))
        .route("/v1/server/friends/platform-match", post(platform_match))
        .route("/v1/friends/{id}", axum::routing::patch(annotate))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AnnotateReq {
    /// Present = set (null clears), absent = keep.
    #[serde(default, deserialize_with = "deserialize_some")]
    note: Option<Option<String>>,
    favorite: Option<bool>,
}

fn deserialize_some<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

async fn annotate(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<AccountId>,
    Json(b): Json<AnnotateReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.social
        .annotate(me.account_id, id, b.note, b.favorite)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LimitQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

async fn recent(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LimitQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "players": s.social.recent(me.account_id, q.limit).await? }),
    ))
}

async fn suggestions(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LimitQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "suggestions": s.social.suggestions(me.account_id, q.limit).await? }),
    ))
}

#[derive(Deserialize)]
struct Ids {
    #[serde(default)]
    ids: Vec<AccountId>,
    #[serde(default)]
    usernames: Vec<String>,
}

async fn resolve(s: &AppState, b: Ids) -> Result<Vec<AccountId>, ApiError> {
    let mut ids = b.ids;
    if b.usernames.len() > musubi_social::MAX_BATCH {
        return Err(musubi_core::Error::InvalidArgument(format!(
            "at most {} usernames per call",
            musubi_social::MAX_BATCH
        ))
        .into());
    }
    if !b.usernames.is_empty() {
        let users = s.accounts.get_users(&[], &b.usernames).await?;
        if users.len() != b.usernames.len() {
            return Err(musubi_core::Error::NotFound("unknown username".into()).into());
        }
        ids.extend(users.into_iter().map(|u| u.id));
    }
    Ok(ids)
}

/// Platform identifier bounds for `/v1/friends/import` (a Steam id is 17 digits, an OIDC
/// subject is at most 255 bytes per the spec).
const MAX_PROVIDER_LEN: usize = 64;
const MAX_SUBJECT_LEN: usize = 256;

#[derive(Deserialize)]
struct ImportBody {
    /// Identity provider the ids belong to: `steam`, `xbox`, `psn`, `nintendo`, `oidc:<name>`, …
    provider: String,
    /// Platform ids of the player's friends, as the platform SDK reports them.
    subjects: Vec<String>,
    /// Also send friend requests to every match.
    #[serde(default)]
    add: bool,
}

/// Platform friends import: the console/launcher friend list only exists on the client, so it
/// sends the platform ids and the server maps them to accounts here.
async fn import(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<ImportBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.subjects.len() > 5000 {
        return Err(musubi_core::Error::InvalidArgument("at most 5000 subjects".into()).into());
    }
    if b.provider.is_empty() || b.provider.len() > MAX_PROVIDER_LEN {
        return Err(musubi_core::Error::InvalidArgument(format!(
            "provider: 1..={MAX_PROVIDER_LEN} bytes"
        ))
        .into());
    }
    if b.subjects
        .iter()
        .any(|x| x.is_empty() || x.len() > MAX_SUBJECT_LEN)
    {
        return Err(musubi_core::Error::InvalidArgument(format!(
            "subjects: each 1..={MAX_SUBJECT_LEN} bytes"
        ))
        .into());
    }
    crate::platform_friends::remember(&s, me.account_id, &b.provider, &b.subjects, b.add).await?;
    let found = s.accounts.by_identities(&b.provider, &b.subjects).await?;
    for (subject, a) in found.iter().filter(|(_, a)| a.id != me.account_id) {
        crate::platform_friends::mark_matched(&s, me.account_id, &b.provider, subject, a.id)
            .await?;
    }
    let matches: Vec<serde_json::Value> = found
        .iter()
        .filter(|(_, a)| a.id != me.account_id && a.disabled_at.is_none())
        .map(|(subject, a)| {
            json!({
                "subject": subject,
                "account_id": a.id,
                "username": a.username,
                "display_name": a.display_name,
            })
        })
        .collect();
    let mut added = 0usize;
    if b.add && !matches.is_empty() {
        let ids: Vec<musubi_core::AccountId> = found
            .iter()
            .filter(|(_, a)| a.id != me.account_id && a.disabled_at.is_none())
            .map(|(_, a)| a.id)
            .collect();
        let outcomes = s.social.add(me.account_id, &ids).await?;
        added = outcomes.len();
        s.events
            .emit(
                "friend.added",
                Some(me.account_id),
                json!({ "ids": ids, "outcomes": outcomes, "import": b.provider }),
            )
            .await;
    }
    Ok(Json(json!({ "matches": matches, "added": added })))
}

#[derive(Deserialize)]
struct ImportListQ {
    provider: Option<String>,
}

/// Stored platform friend ids and who they resolved to (matches made later show up here too).
async fn import_list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ImportListQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        crate::platform_friends::list(&s, me.account_id, q.provider.as_deref()).await?,
    ))
}

/// Run the platform friend matcher now instead of waiting for the leader tick.
async fn platform_match(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = crate::platform_friends::run_match(&s, 5000).await?;
    Ok(Json(json!({ "matched": n })))
}

async fn add(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    add_targets(&s, me.account_id, ids, false).await
}

/// Shared by `POST /v1/friends` and add-by-friend-code: invites/accepts, events, notifications.
pub(super) async fn add_targets(
    s: &AppState,
    me_id: AccountId,
    ids: Vec<AccountId>,
    via_code: bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.age.check(me_id, "friends")?;
    let outcomes = s.social.add_via(me_id, &ids, via_code).await?;
    s.events
        .emit(
            "friend.added",
            Some(me_id),
            json!({ "ids": ids, "outcomes": outcomes }),
        )
        .await;
    if !outcomes.is_empty() {
        let username = s
            .accounts
            .get_users(&[me_id], &[])
            .await?
            .pop()
            .map(|u| u.username)
            .unwrap_or_default();
        let batch = outcomes
            .into_iter()
            .map(|(other, accepted)| NewNotification {
                user_id: other,
                sender_id: Some(me_id),
                subject: if accepted {
                    format!("{username} accepted your friend request")
                } else {
                    format!("{username} wants to add you as a friend")
                },
                content: json!({ "username": username }),
                code: if accepted {
                    code::FRIEND_ACCEPTED
                } else {
                    code::FRIEND_REQUEST
                },
                persistent: true,
                ttl_secs: None,
            })
            .collect();
        s.notify.send(batch).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn remove(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    s.social.delete(me.account_id, &ids).await?;
    s.events
        .emit("friend.removed", Some(me.account_id), json!({ "ids": ids }))
        .await;
    Ok(Json(json!({ "ok": true })))
}

async fn block(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    s.social.block(me.account_id, &ids).await?;
    if let Some(v) = s.voice.clone().filter(|_| s.cfg.aurix.moderation_sync) {
        // the block also silences them in voice; Aurix is a side channel, so best effort
        let (me_id, ids) = (me.account_id, ids.clone());
        tokio::spawn(async move {
            for id in ids {
                if let Err(e) = v.block(&me_id.to_string(), &id.to_string()).await {
                    tracing::warn!(error = %e, "aurix block sync failed");
                }
            }
        });
    }
    s.events
        .emit("friend.blocked", Some(me.account_id), json!({ "ids": ids }))
        .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    state: Option<i16>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_limit() -> usize {
    50
}

async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = match q.state {
        None => None,
        Some(0) => Some(FriendState::Friend),
        Some(1) => Some(FriendState::InviteSent),
        Some(2) => Some(FriendState::InviteReceived),
        Some(3) => Some(FriendState::Blocked),
        Some(_) => {
            return Err(musubi_core::Error::InvalidArgument("state must be 0..=3".into()).into())
        }
    };
    let list: FriendList = s
        .social
        .list(me.account_id, state, q.limit, q.cursor.as_deref())
        .await?;
    let mut v = serde_json::to_value(list).map_err(musubi_core::Error::internal)?;
    if let Some(friends) = v["friends"].as_array_mut() {
        for f in friends {
            let online = f["user"]["id"]
                .as_str()
                .and_then(|id| id.parse::<uuid::Uuid>().ok())
                .is_some_and(|id| s.fanout.is_online_anywhere(musubi_core::AccountId(id)));
            f["online"] = serde_json::Value::Bool(online);
        }
    }
    Ok(Json(v))
}

/// Postgres fixture for the audit tests of this module's neighbours (`MUSUBI_TEST_DATABASE_URL`;
/// tests return early when it is unset, like the integration suite).
#[cfg(test)]
pub(crate) mod test_db {
    use sqlx::PgPool;

    pub(crate) async fn pool() -> Option<PgPool> {
        let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
        let cfg = musubi_core::config::DatabaseConfig {
            url,
            max_connections: 4,
            run_migrations: true,
        };
        let db = crate::db::connect(&cfg).await.expect("connect");
        static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
        ONCE.get_or_init(|| async {
            crate::db::migrate(&db).await.expect("migrate");
        })
        .await;
        Some(db)
    }

    pub(crate) async fn account(db: &PgPool) -> musubi_core::AccountId {
        let id = uuid::Uuid::now_v7();
        sqlx::query("insert into musubi.accounts (id, username) values ($1, $2)")
            .bind(id)
            .bind(format!("audit_{}", id.simple()))
            .execute(db)
            .await
            .expect("account");
        musubi_core::AccountId(id)
    }
}
