//! `Idempotency-Key`: a mutating request (POST/PUT/PATCH/DELETE) carrying the header is executed
//! once per caller + method + path + key; repeats within 24h get the stored response back with
//! `Idempotent-Replayed: true`, a concurrent duplicate gets 409. Mobile clients retrying a
//! purchase or a write over a flaky link therefore never double-apply it.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use musubi_core::Error;

use crate::app::AppState;
use crate::http::ApiError;

const MAX_STORED_BODY: usize = 1 << 20;

fn key_of(req: &Request) -> Option<String> {
    let k = req.headers().get("idempotency-key")?.to_str().ok()?.trim();
    (!k.is_empty() && k.len() <= 128).then(|| k.to_string())
}

pub async fn replay(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let mutating = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let Some(key) = key_of(&req).filter(|_| mutating) else {
        return next.run(req).await;
    };
    let caller = req
        .headers()
        .get("authorization")
        .or_else(|| req.headers().get("x-musubi-server-key"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hash = crate::keys::hash(&format!(
        "{caller}\n{}\n{}\n{key}",
        req.method(),
        req.uri().path()
    ));
    let claimed = sqlx::query(
        "insert into musubi.idempotency (key_hash, status) values ($1, 0) on conflict do nothing",
    )
    .bind(&hash)
    .execute(&s.db)
    .await
    .map(|r| r.rows_affected() > 0);
    match claimed {
        Err(e) => return ApiError(Error::internal(e)).into_response(),
        Ok(false) => {
            let row: Option<(i32, Option<String>, Option<Vec<u8>>)> = sqlx::query_as(
                "select status, content_type, body from musubi.idempotency where key_hash = $1",
            )
            .bind(&hash)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
            return match row {
                Some((0, _, _)) | None => ApiError(Error::Conflict(
                    "a request with this Idempotency-Key is still in flight".into(),
                ))
                .into_response(),
                Some((status, ct, body)) => {
                    let mut res = Response::builder()
                        .status(StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK))
                        .header("idempotent-replayed", "true");
                    if let Some(ct) = ct {
                        res = res.header("content-type", ct);
                    }
                    res.body(Body::from(body.unwrap_or_default()))
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
                }
            };
        }
        Ok(true) => {}
    }
    let res = next.run(req).await;
    if res.status().is_server_error() {
        let _ = sqlx::query("delete from musubi.idempotency where key_hash = $1")
            .bind(&hash)
            .execute(&s.db)
            .await;
        return res;
    }
    let (parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_STORED_BODY).await {
        Ok(b) => b,
        Err(_) => {
            let _ = sqlx::query("delete from musubi.idempotency where key_hash = $1")
                .bind(&hash)
                .execute(&s.db)
                .await;
            return ApiError(Error::Internal("response too large to store".into())).into_response();
        }
    };
    let ct = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let _ = sqlx::query(
        "update musubi.idempotency set status = $2, content_type = $3, body = $4 where key_hash = $1",
    )
    .bind(&hash)
    .bind(parts.status.as_u16() as i32)
    .bind(ct)
    .bind(bytes.as_ref())
    .execute(&s.db)
    .await;
    Response::from_parts(parts, Body::from(bytes))
}
