//! Client-facing LiveOps (Zanshin) and voice (Aurix) endpoints. The Musubi account id is the
//! identity on both sides; the server keys never leave this process.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/liveops/flags", get(flags))
        .route("/v1/liveops/live-events", get(live_events))
        .route("/v1/voice/token", post(voice_token))
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
    let l = s
        .liveops
        .as_ref()
        .ok_or_else(|| Error::NotFound("liveops (zanshin) not configured".into()))?;
    let names: Vec<String> = q
        .names
        .map(|n| {
            n.split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(
        json!({ "flags": l.flags(&me.account_id.to_string(), &names).await? }),
    ))
}

async fn live_events(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let l = s
        .liveops
        .as_ref()
        .ok_or_else(|| Error::NotFound("liveops (zanshin) not configured".into()))?;
    Ok(Json(l.live_events(&me.account_id.to_string()).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VoiceTokenReq {
    party_id: Option<String>,
    match_id: Option<String>,
    display_name: Option<String>,
    #[serde(default = "yes")]
    speak: bool,
}

fn yes() -> bool {
    true
}

/// Voice channel per party or match; only current members get a token (checked on this node).
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
    let (channel, kind, max) = match (&r.party_id, &r.match_id) {
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
            return Err(Error::InvalidArgument("party_id or match_id required".into()).into())
        }
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
