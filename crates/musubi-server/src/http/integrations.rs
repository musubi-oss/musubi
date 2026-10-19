//! Client-facing LiveOps (Zanshin) and voice (Aurix) endpoints. The Musubi account id is the
//! identity on both sides; the server keys never leave this process.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::extract::{Bearer, ServerKey};
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/liveops/flags", get(flags))
        .route("/v1/liveops/live-events", get(live_events))
        .route(
            "/v1/server/liveops/changed",
            axum::routing::post(liveops_changed),
        )
        .route("/v1/voice/token", post(voice_token))
        .route("/v1/voice/report", post(voice_report))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FlagsQuery {
    names: Option<String>,
}

async fn flags(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<FlagsQuery>,
) -> Result<Json<Value>, ApiError> {
    let names: Vec<String> = q
        .names
        .map(|n| {
            n.split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let key = format!("liveops.flags|{}|{}", me.account_id, names.join(","));
    if let Some(v) = cached(&s, &key) {
        return Ok(Json(json!({ "flags": v })));
    }
    let v = crate::liveops::merged_flags(&s, me.account_id, &names).await?;
    remember(&s, key, v.clone());
    Ok(Json(json!({ "flags": v })))
}

fn cached(s: &AppState, key: &str) -> Option<Value> {
    let cache = s.liveops_cache.lock().unwrap();
    cache
        .get(key)
        .filter(|(at, _)| at.elapsed() < crate::runtime_host::LIVEOPS_TTL)
        .map(|(_, v)| v.clone())
}

fn remember(s: &AppState, key: String, v: Value) {
    let mut cache = s.liveops_cache.lock().unwrap();
    cache.retain(|_, (at, _)| at.elapsed() < crate::runtime_host::LIVEOPS_TTL);
    cache.insert(key, (std::time::Instant::now(), v));
}

/// Same cache and key format as the module host ops, so one `liveops/changed` clears both.
/// Native live events (`musubi.live_events`) plus Zanshin's when configured.
async fn live_events(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let key = format!("liveops.live_events|{}|", me.account_id);
    if let Some(v) = cached(&s, &key) {
        return Ok(Json(v));
    }
    let v = crate::liveops::merged_live_events(&s, me.account_id).await?;
    remember(&s, key, v.clone());
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VoiceTokenReq {
    party_id: Option<String>,
    match_id: Option<String>,
    /// Proximity channel of a world the caller is subscribed to (on this node).
    world: Option<String>,
    display_name: Option<String>,
    #[serde(default = "yes")]
    speak: bool,
}

fn yes() -> bool {
    true
}

/// Voice channel per party, match or world; only current members get a token (checked on this
/// node). A world's channel is positional: Aurix attenuates by the positions the client reports.
async fn voice_token(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<VoiceTokenReq>,
) -> Result<Json<Value>, ApiError> {
    let v = s
        .voice
        .as_ref()
        .ok_or_else(|| Error::NotFound("voice (aurix) not configured".into()))?;
    // socket sessions of this account on this node (the HTTP session id is not the socket's)
    let sids: Vec<_> = s
        .hub
        .presences_of(me.account_id)
        .into_iter()
        .map(|p| p.session_id)
        .collect();
    let world_grant = match &r.world {
        Some(w) => {
            if !sids.iter().any(|sid| s.worlds.is_subscribed(w, *sid)) {
                return Err(Error::PermissionDenied(
                    "not subscribed to that world (or world not on this node)".into(),
                )
                .into());
            }
            Some((
                format!("world-{w}"),
                s.cfg.aurix.world_channel_type.clone(),
                4096,
            ))
        }
        None => None,
    };
    let (channel, kind, max) = match world_grant {
        Some(g) => g,
        None => match (&r.party_id, &r.match_id) {
            (Some(p), _) => {
                let members = s.parties.member_sessions(p);
                if !sids.iter().any(|sid| members.contains(sid)) {
                    return Err(Error::PermissionDenied(
                        "not in that party (or party not on this node)".into(),
                    )
                    .into());
                }
                (
                    format!("party-{p}"),
                    s.cfg.aurix.party_channel_type.clone(),
                    256,
                )
            }
            (None, Some(m)) => {
                if !sids.iter().any(|sid| s.matches.is_member(*sid, m)) {
                    return Err(Error::PermissionDenied(
                        "not in that match (or match not on this node)".into(),
                    )
                    .into());
                }
                (
                    format!("match-{m}"),
                    s.cfg.aurix.match_channel_type.clone(),
                    256,
                )
            }
            (None, None) => {
                return Err(
                    Error::InvalidArgument("party_id, match_id or world required".into()).into(),
                )
            }
        },
    };
    let kind = if kind.is_empty() {
        "team".to_string()
    } else {
        kind
    };
    let display = r.display_name.unwrap_or_else(|| me.account_id.to_string());
    let grant = v
        .token_for_channel(
            &me.account_id.to_string(),
            &display,
            &channel,
            &kind,
            max,
            r.speak,
        )
        .await?;
    Ok(Json(serde_json::to_value(grant).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct VoiceReportReq {
    target_account_id: uuid::Uuid,
    channel_id: Option<String>,
    reason: String,
}

/// Report a player for what they said in voice: filed in Aurix, where the recording and
/// transcript evidence lives, under the caller's identity.
async fn voice_report(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<VoiceReportReq>,
) -> Result<Json<Value>, ApiError> {
    let v = s
        .voice
        .as_ref()
        .ok_or_else(|| Error::NotFound("voice (aurix) not configured".into()))?;
    if r.reason.trim().is_empty() {
        return Err(Error::InvalidArgument("reason required".into()).into());
    }
    let out = v
        .report(
            &me.account_id.to_string(),
            &r.target_account_id.to_string(),
            r.channel_id.as_deref(),
            &r.reason,
        )
        .await?;
    s.events
        .emit(
            "voice.reported",
            Some(me.account_id),
            json!({ "target": r.target_account_id, "channel_id": r.channel_id }),
        )
        .await;
    Ok(Json(out))
}

/// `POST /v1/server/liveops/changed` — Zanshin's change webhook (any body): cached flags / live
/// events are dropped on every node and modules get `hook_liveops_changed` with the body.
async fn liveops_changed(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let payload = body.map(|Json(v)| v).unwrap_or(serde_json::Value::Null);
    crate::runtime_host::liveops_changed(&s, payload, true).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
