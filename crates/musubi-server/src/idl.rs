//! Deployed IDL (plan 3.8): one TOML schema → storage collection contracts, `/v1/rpc/{name}`
//! request validation and OpenAPI. Stored in `musubi.idl`; one document per cluster.

use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use musubi_idl::Idl;
use musubi_storage::{CollectionSchema, StorageService};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

#[derive(Clone)]
pub struct IdlRegistry {
    db: PgPool,
    storage: StorageService,
    current: Arc<RwLock<Option<Arc<Idl>>>>,
}

fn bad(e: musubi_idl::IdlError) -> Error {
    Error::InvalidArgument(e.0)
}

fn db(e: sqlx::Error) -> Error {
    Error::Internal(e.to_string())
}

impl IdlRegistry {
    pub fn new(db: PgPool, storage: StorageService) -> Self {
        Self {
            db,
            storage,
            current: Arc::new(RwLock::new(None)),
        }
    }

    pub fn get(&self) -> Option<Arc<Idl>> {
        self.current.read().ok().and_then(|g| g.clone())
    }

    /// Load the deployed document (if any) from the database.
    pub async fn load(&self) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("select source from musubi.idl where name = 'default'")
                .fetch_optional(&self.db)
                .await
                .map_err(db)?;
        match row {
            Some((src,)) => {
                let idl = Idl::parse(&src).map_err(bad)?;
                *self.current.write().unwrap() = Some(Arc::new(idl));
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Parse, attach every `[collections.*]` as a storage contract (same rules as
    /// `PUT /v1/admin/storage/schemas`: version bumps need migrations), persist and activate.
    pub async fn apply(&self, source: &str) -> Result<Value> {
        let idl = Idl::parse(source).map_err(bad)?;
        let mut collections = Vec::new();
        for c in idl.collection_schemas().map_err(bad)? {
            let s: CollectionSchema = serde_json::from_value(c)
                .map_err(|e| Error::InvalidArgument(format!("collection contract: {e}")))?;
            let applied = self.storage.put_schema(s).await?;
            collections
                .push(json!({ "collection": applied.collection, "version": applied.version }));
        }
        sqlx::query(
            "insert into musubi.idl (name, source) values ('default', $1)
             on conflict (name) do update set source = excluded.source, updated_at = now()",
        )
        .bind(source)
        .execute(&self.db)
        .await
        .map_err(db)?;
        let summary = summary(&idl, &collections);
        *self.current.write().unwrap() = Some(Arc::new(idl));
        Ok(summary)
    }

    /// Drop the deployed document; storage contracts it attached stay in place.
    pub async fn clear(&self) -> Result<bool> {
        let n = sqlx::query("delete from musubi.idl where name = 'default'")
            .execute(&self.db)
            .await
            .map_err(db)?
            .rows_affected();
        *self.current.write().unwrap() = None;
        Ok(n > 0)
    }

    /// `/v1/rpc/{name}`: enforce the declared `auth` and validate the JSON body against
    /// `input`. RPCs the IDL does not mention pass through untouched.
    pub fn check_rpc(&self, name: &str, server: bool, body: &[u8]) -> Result<()> {
        let Some(idl) = self.get() else { return Ok(()) };
        let Some(rpc) = idl.rpcs.get(name) else {
            return Ok(());
        };
        match rpc.auth {
            musubi_idl::Auth::Server if !server => {
                return Err(Error::PermissionDenied(format!(
                    "rpc `{name}` is server-only"
                )))
            }
            musubi_idl::Auth::Player if server => {
                return Err(Error::PermissionDenied(format!(
                    "rpc `{name}` is player-only"
                )))
            }
            _ => {}
        }
        if let Some(schema) = idl.rpc_input_schema(name) {
            let v: Value = if body.is_empty() {
                json!({})
            } else {
                serde_json::from_slice(body)
                    .map_err(|e| Error::InvalidArgument(format!("rpc `{name}` input: {e}")))?
            };
            musubi_storage::schema::validate(&schema, &v)
                .map_err(|e| Error::InvalidArgument(format!("rpc `{name}` input: {e}")))?;
        }
        Ok(())
    }
}

fn summary(idl: &Idl, collections: &[Value]) -> Value {
    json!({
        "package": idl.package,
        "types": idl.types.keys().collect::<Vec<_>>(),
        "enums": idl.enums.keys().collect::<Vec<_>>(),
        "collections": collections,
        "rpcs": idl.rpcs.iter().map(|(n, r)| json!({
            "name": n, "input": r.input, "output": r.output, "auth": r.auth
        })).collect::<Vec<_>>(),
    })
}

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/admin/idl", get(show).put(put).delete(delete))
}

#[derive(Deserialize, Default)]
pub struct ShowQuery {
    /// `source` (default, TOML), `summary`, `schemas` (storage contracts), `openapi`
    /// (components + paths), `rust`.
    #[serde(default)]
    pub format: Option<String>,
}

async fn show(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ShowQuery>,
) -> std::result::Result<Response, ApiError> {
    let idl = s
        .idl
        .get()
        .ok_or_else(|| Error::NotFound("no IDL deployed".into()))?;
    let format = q.format.as_deref().unwrap_or("source");
    Ok(match format {
        "source" => {
            let (src,): (String,) =
                sqlx::query_as("select source from musubi.idl where name = 'default'")
                    .fetch_one(&s.db)
                    .await
                    .map_err(db)?;
            ([(CONTENT_TYPE, "application/toml")], src).into_response()
        }
        "summary" => Json(summary(&idl, &collections_of(&idl))).into_response(),
        "schemas" => {
            Json(json!({ "schemas": idl.collection_schemas().map_err(bad)? })).into_response()
        }
        "openapi" => Json(json!({
            "components": { "schemas": idl.openapi_components() },
            "paths": idl.openapi_paths(),
        }))
        .into_response(),
        "rust" => ([(CONTENT_TYPE, "text/x-rust")], idl.rust()).into_response(),
        other => {
            return Err(Error::InvalidArgument(format!("unknown format `{other}`")).into());
        }
    })
}

fn collections_of(idl: &Idl) -> Vec<Value> {
    idl.collections
        .iter()
        .map(|(n, c)| json!({ "collection": n, "version": c.version }))
        .collect()
}

/// Body: the TOML document (`Content-Type: application/toml` or JSON `{"source": "..."}`).
async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<Value>, ApiError> {
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));
    let source = if is_json {
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::InvalidArgument(format!("body: {e}")))?;
        v.get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidArgument("`source` (TOML string) is required".into()))?
            .to_string()
    } else {
        String::from_utf8(body.to_vec())
            .map_err(|_| Error::InvalidArgument("body must be UTF-8 TOML".into()))?
    };
    if source.trim().is_empty() {
        return Err(Error::InvalidArgument("empty IDL".into()).into());
    }
    Ok(Json(s.idl.apply(&source).await?))
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let removed = s.idl.clear().await?;
    Ok(Json(json!({ "removed": removed })))
}
