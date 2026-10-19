//! P1-30: operator view of secrets at rest — which master keys are loaded, which config values
//! came from `${env:…}`/`${file:…}`, how many stored credentials are sealed/plaintext, and
//! re-encryption after a key rotation.
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{secrets, Error};
use serde_json::{json, Value};

use super::error::ApiError;
use super::extract::ServerKey;
use super::AppState;

/// `(table, secret column, text id expression)` of every credential store.
const STORES: [(&str, &str, &str); 2] = [
    ("webhooks", "secret", "id::text"),
    ("auth_providers", "client_secret", "name"),
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/secrets/status", get(status))
        .route("/v1/admin/secrets/reencrypt", post(reencrypt))
}

async fn status(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let kids = secrets::kids();
    let current = secrets::current_kid();
    let mut stores = Vec::new();
    let mut plaintext_total = 0i64;
    for (table, col, _) in STORES {
        let values: Vec<String> = sqlx::query_scalar(&format!("select {col} from musubi.{table}"))
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
        let (mut sealed_current, mut sealed_other, mut sealed_unknown, mut plaintext, mut empty) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        for v in &values {
            if v.is_empty() {
                empty += 1;
            } else if let Some(k) = secrets::sealed_kid(v) {
                if Some(k) == current.as_deref() {
                    sealed_current += 1;
                } else if kids.iter().any(|x| x == k) {
                    sealed_other += 1;
                } else {
                    sealed_unknown += 1;
                }
            } else {
                plaintext += 1;
            }
        }
        plaintext_total += plaintext;
        stores.push(json!({
            "table": table, "column": col, "rows": values.len(), "sealed_current": sealed_current,
            "sealed_other_key": sealed_other, "sealed_unknown_key": sealed_unknown,
            "plaintext": plaintext, "empty": empty
        }));
    }
    let mut warnings = Vec::new();
    if current.is_none() {
        warnings.push("no [security] master_keys configured: stored credentials are plaintext");
    } else if plaintext_total > 0 {
        warnings.push("plaintext credentials remain: run POST /v1/admin/secrets/reencrypt");
    }
    if stores
        .iter()
        .any(|st| st["sealed_unknown_key"].as_i64().unwrap_or(0) > 0)
    {
        warnings.push("some credentials are sealed with a key that is not loaded");
    }
    if stores
        .iter()
        .any(|st| st["sealed_other_key"].as_i64().unwrap_or(0) > 0)
    {
        warnings.push("some credentials still use a previous key: run POST /v1/admin/secrets/reencrypt before dropping it");
    }
    Ok(Json(json!({
        "configured": current.is_some(), "current_key": current, "keys": kids,
        "config_refs": s.cfg.resolved_refs, "stores": stores, "warnings": warnings
    })))
}

/// Re-seal every stored credential with the current master key (plaintext → sealed, previous key →
/// current key). Values sealed with a key that is not loaded are reported, not touched.
async fn reencrypt(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let Some(current) = secrets::current_kid() else {
        return Err(Error::InvalidArgument("no [security] master_keys configured".into()).into());
    };
    let mut report = Vec::new();
    for (table, col, id_expr) in STORES {
        let rows: Vec<(String, String)> =
            sqlx::query_as(&format!("select {id_expr}, {col} from musubi.{table}"))
                .fetch_all(&s.db)
                .await
                .map_err(Error::internal)?;
        let (mut resealed, mut skipped, mut failed) = (0i64, 0i64, Vec::new());
        for (id, value) in rows {
            if value.is_empty() || secrets::sealed_kid(&value) == Some(current.as_str()) {
                skipped += 1;
                continue;
            }
            let plain = match secrets::open(&value) {
                Ok(p) => p,
                Err(e) => {
                    failed.push(json!({ "id": id, "error": e }));
                    continue;
                }
            };
            sqlx::query(&format!(
                "update musubi.{table} set {col} = $2 where {id_expr} = $1"
            ))
            .bind(&id)
            .bind(secrets::seal(&plain))
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
            resealed += 1;
        }
        report.push(json!({ "table": table, "column": col, "resealed": resealed, "skipped": skipped, "failed": failed }));
    }
    s.events
        .emit(
            "secrets.reencrypted",
            None,
            json!({ "key": current, "stores": report }),
        )
        .await;
    Ok(Json(json!({ "key": current, "stores": report })))
}
