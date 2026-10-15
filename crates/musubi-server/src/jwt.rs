//! Managed JWT signing keys: generated and rotated through the admin API, stored in
//! `musubi.jwt_keys`, installed into the token signer on every node (bus topic `admin.jwt`).

use std::sync::Arc;

use musubi_auth::token::{JwtKey, TokenSigner};
use musubi_auth::AuthService;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;

pub const JWT_TOPIC: &str = "admin.jwt";

/// Load every key (newest first) into the signer; keys retired longer than `access_ttl + 1h`
/// are deleted since no live token can carry them.
pub async fn load(db: &PgPool, signer: &TokenSigner) -> Result<usize> {
    sqlx::query(
        "delete from musubi.jwt_keys where retired_at is not null
           and retired_at < now() - make_interval(secs => $1)",
    )
    .bind(signer.access_ttl_secs as f64 + 3600.0)
    .execute(db)
    .await
    .map_err(Error::internal)?;
    let rows: Vec<(String, Vec<u8>, bool)> = sqlx::query_as(
        "select kid, pkcs8, retired_at is not null from musubi.jwt_keys order by created_at desc",
    )
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let n = rows.len();
    signer.install(
        rows.into_iter()
            .map(|(kid, pkcs8, retired)| JwtKey {
                kid,
                pkcs8,
                retired,
            })
            .collect(),
    )?;
    Ok(n)
}

pub async fn rotate(db: &PgPool, auth: &AuthService) -> Result<String> {
    let (kid, pkcs8) = TokenSigner::generate_key()?;
    sqlx::query("insert into musubi.jwt_keys (kid, pkcs8) values ($1, $2)")
        .bind(&kid)
        .bind(&pkcs8)
        .execute(db)
        .await
        .map_err(Error::internal)?;
    load(db, auth.signer()).await?;
    Ok(kid)
}

pub async fn retire(db: &PgPool, auth: &AuthService, kid: &str) -> Result<bool> {
    let n = sqlx::query(
        "update musubi.jwt_keys set retired_at = now() where kid = $1 and retired_at is null",
    )
    .bind(kid)
    .execute(db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n > 0 {
        load(db, auth.signer()).await?;
    }
    Ok(n > 0)
}

pub async fn list(db: &PgPool, auth: &AuthService) -> Result<Value> {
    let rows: Vec<(String, time::OffsetDateTime, Option<time::OffsetDateTime>)> = sqlx::query_as(
        "select kid, created_at, retired_at from musubi.jwt_keys order by created_at desc",
    )
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let active = auth.signer().active_kid();
    Ok(json!({
        "active": active,
        "keys": rows.into_iter().map(|(kid, c, r)| json!({
            "kid": kid,
            "created_at": c.format(&time::format_description::well_known::Rfc3339).ok(),
            "retired_at": r.and_then(|r| r.format(&time::format_description::well_known::Rfc3339).ok()),
        })).collect::<Vec<_>>(),
    }))
}

/// Reinstall keys when another node rotates or retires.
pub fn start(cluster: &Arc<musubi_cluster::Cluster>, db: PgPool, auth: AuthService) {
    let mut rx = cluster.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == JWT_TOPIC => {
                    if let Err(e) = load(&db, auth.signer()).await {
                        tracing::warn!(error = %e, "jwt keys reload failed");
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}
