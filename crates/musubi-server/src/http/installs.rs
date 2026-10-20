//! Install-script records (GameLift `install.sh` parity): what `spec.install.command` did on
//! each node/host per build, and a reset that makes the next start run it again.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/fleets/{id}/installs", get(list).delete(reset))
        .route(
            "/v1/server/fleets/{id}/installs/reset",
            axum::routing::post(reset),
        )
}

async fn list(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let f = s.fleets.get(id).await?;
    let runs = s.fleets.installs(id).await?;
    let failed = runs.iter().filter(|r| r["ok"] == false).count();
    Ok(Json(json!({
        "fleet_id": id,
        "install": f.spec.install,
        "install_hash": f.spec.install.as_ref().map(|i| i.hash()),
        "runs": runs,
        "total": runs.len(),
        "failed": failed,
    })))
}

async fn reset(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = s.fleets.reset_installs(id).await?;
    Ok(Json(json!({ "fleet_id": id, "cleared": n })))
}
