//! Legacy key-value storage: `collection / key / owner` → JSON object, with Nakama-compatible
//! permission codes and optimistic concurrency via content-hash versions.
//!
//! Permissions: read `0` = nobody (server only), `1` = owner, `2` = public.
//! Write `0` = nobody (server only), `1` = owner.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub mod schema;
pub use schema::CollectionSchema;

pub const SYSTEM_OWNER: Uuid = Uuid::nil();
pub const MAX_VALUE_BYTES: usize = 1 << 20;
pub const MAX_BATCH: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    pub collection: String,
    pub key: String,
    pub owner_id: Uuid,
    pub value: serde_json::Value,
    pub version: String,
    pub read_permission: i16,
    pub write_permission: i16,
    /// Schema version the stored value conforms to (0 = no schema).
    #[serde(default)]
    pub schema_version: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Set when the object was written with `ttl_secs`; expired objects are never returned.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteOp {
    pub collection: String,
    pub key: String,
    pub value: serde_json::Value,
    /// `None` = unconditional; `Some("*")` = only if absent; `Some(hash)` = only if current version matches.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "one")]
    pub read_permission: i16,
    #[serde(default = "one")]
    pub write_permission: i16,
    /// Time to live in seconds; the object disappears from reads and is purged after it elapses.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRef {
    pub collection: String,
    pub key: String,
    #[serde(default)]
    pub owner_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteOp {
    pub collection: String,
    pub key: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ack {
    pub collection: String,
    pub key: String,
    pub owner_id: Uuid,
    pub version: String,
}

/// Who performs the operation. `Server` bypasses permission checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    User(AccountId),
    Server,
}

/// One committed write/patch/delete, handed to the change hook (live subscriptions).
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub collection: String,
    pub key: String,
    pub owner_id: Uuid,
    pub version: Option<String>,
    pub value: Option<serde_json::Value>,
    pub schema_version: i32,
    pub deleted: bool,
}

type ChangeHook = std::sync::Arc<dyn Fn(Change) + Send + Sync>;

#[derive(Clone)]
pub struct StorageService {
    db: PgPool,
    max_objects: i64,
    max_bytes: i64,
    schemas: std::sync::Arc<
        std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<CollectionSchema>>>,
    >,
    on_change: std::sync::Arc<std::sync::RwLock<Option<ChangeHook>>>,
}

/// JSON Merge Patch (RFC 7386) against one object; OCC like `WriteOp::version`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchOp {
    pub collection: String,
    pub key: String,
    pub patch: serde_json::Value,
    #[serde(default)]
    pub version: Option<String>,
}

/// RFC 7386: objects merge recursively, `null` removes a member, anything else replaces.
pub fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match patch {
        serde_json::Value::Object(p) => {
            if !target.is_object() {
                *target = serde_json::Value::Object(Default::default());
            }
            let t = target.as_object_mut().unwrap();
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                }
            }
        }
        other => *target = other.clone(),
    }
}

type Row = (
    String,
    String,
    Uuid,
    serde_json::Value,
    String,
    i16,
    i16,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
    i32,
);

const COLS: &str = "collection, key, owner_id, value, version, read_permission, write_permission, created_at, updated_at, expires_at, schema_version";

/// Optional narrowing for `StorageService::list_where`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListFilter<'a> {
    /// Opaque cursor from a previous page.
    pub cursor: Option<&'a str>,
    /// JSONB containment filter (`value @> filter`).
    pub filter: Option<&'a serde_json::Value>,
    /// Only keys starting with this prefix (literal match).
    pub key_prefix: Option<&'a str>,
    /// Order by a field of the value (`score`, nested `stats.wins`), `-field` for descending;
    /// without it objects come in `(owner, key)` order.
    pub sort: Option<&'a str>,
}

fn visibility(caller: Caller, owner_id: Option<Uuid>) -> (Option<Uuid>, i16) {
    match (caller, owner_id) {
        (Caller::Server, o) => (o, 0),
        (Caller::User(a), None) => (Some(a.0), 1),
        (Caller::User(a), Some(o)) if o == a.0 => (Some(o), 1),
        (Caller::User(_), Some(o)) => (Some(o), 2),
    }
}

fn like_prefix(key_prefix: Option<&str>) -> Option<String> {
    key_prefix.filter(|p| !p.is_empty()).map(|p| {
        format!(
            "{}%",
            p.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}

/// Cursor of a sorted page: the sort value, owner and key of the last row.
fn encode_sorted_cursor(sort_value: &serde_json::Value, owner: Uuid, key: &str) -> String {
    use base64::Engine;
    let v = serde_json::json!([sort_value, owner, key]);
    format!(
        "s:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    )
}

fn decode_sorted_cursor(c: &str) -> Result<(serde_json::Value, Uuid, String)> {
    use base64::Engine;
    let bad = || Error::InvalidArgument("bad cursor".into());
    let raw = c.strip_prefix("s:").ok_or_else(bad)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| bad())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    let arr = v.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
    let owner: Uuid = arr[1]
        .as_str()
        .ok_or_else(bad)?
        .parse()
        .map_err(|_| bad())?;
    let key = arr[2].as_str().ok_or_else(bad)?.to_string();
    Ok((arr[0].clone(), owner, key))
}

impl StorageService {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            max_objects: 0,
            max_bytes: 0,
            schemas: Default::default(),
            on_change: Default::default(),
        }
    }

    /// Per-user quotas (`0` = unlimited).
    pub fn with_limits(mut self, max_objects: i64, max_bytes: i64) -> Self {
        self.max_objects = max_objects;
        self.max_bytes = max_bytes;
        self
    }

    /// Objects and bytes owned by `owner`.
    pub async fn usage(&self, owner: Uuid) -> Result<(i64, i64)> {
        let r: Option<(i64, i64)> =
            sqlx::query_as("select objects, bytes from musubi.storage_usage where owner_id = $1")
                .bind(owner)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(r.unwrap_or((0, 0)))
    }

    async fn enforce_quota(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        owner: Uuid,
    ) -> Result<()> {
        if owner == SYSTEM_OWNER || (self.max_objects == 0 && self.max_bytes == 0) {
            return Ok(());
        }
        let (objects, bytes): (i64, i64) =
            sqlx::query_as("select objects, bytes from musubi.storage_usage where owner_id = $1")
                .bind(owner)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?
                .unwrap_or((0, 0));
        if self.max_objects > 0 && objects > self.max_objects {
            return Err(Error::ResourceExhausted(format!(
                "storage quota: {} objects per user",
                self.max_objects
            )));
        }
        if self.max_bytes > 0 && bytes > self.max_bytes {
            return Err(Error::ResourceExhausted(format!(
                "storage quota: {} bytes per user",
                self.max_bytes
            )));
        }
        Ok(())
    }

    /// Count objects the caller may see, with the same filters as `list_where`.
    pub async fn count(
        &self,
        caller: Caller,
        collection: &str,
        owner_id: Option<Uuid>,
        f: ListFilter<'_>,
    ) -> Result<i64> {
        validate_name(collection, "collection")?;
        let (filter, ops) = schema::split_where(f.filter)?;
        let (owner_id, min_read) = visibility(caller, owner_id);
        let prefix_like = like_prefix(f.key_prefix);
        let sql = format!(
            "select count(*) from musubi.storage_objects
             where collection = $1 and ($2::uuid is null or owner_id = $2) and read_permission >= $3
               and (expires_at is null or expires_at > now())
               and ($4::jsonb is null or value @> $4)
               and ($5::text is null or key like $5){}",
            schema::where_sql(&ops, 6)
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql)
            .bind(collection)
            .bind(owner_id)
            .bind(min_read)
            .bind(filter)
            .bind(prefix_like);
        for o in &ops {
            q = q.bind(&o.value);
        }
        q.fetch_one(&self.db).await.map_err(Error::internal)
    }

    /// Merge-patch existing objects (missing ones are an error) under the write rules of `write`.
    pub async fn patch(
        &self,
        caller: Caller,
        owner_override: Option<Uuid>,
        ops: &[PatchOp],
    ) -> Result<Vec<Ack>> {
        if ops.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!(
                "at most {MAX_BATCH} objects per patch"
            )));
        }
        let owner = match (caller, owner_override) {
            (Caller::Server, o) => o.unwrap_or(SYSTEM_OWNER),
            (Caller::User(a), Some(o)) if o != a.0 => {
                return Err(Error::PermissionDenied(
                    "cannot patch objects owned by another user".into(),
                ))
            }
            (Caller::User(a), _) => a.0,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut acks = Vec::with_capacity(ops.len());
        let mut changes = Vec::new();
        for op in ops {
            validate_name(&op.collection, "collection")?;
            validate_name(&op.key, "key")?;
            if !op.patch.is_object() {
                return Err(Error::InvalidArgument("patch must be a JSON object".into()));
            }
            let current = sqlx::query_as::<_, (serde_json::Value, String, i16, i32)>(
                "select value, version, write_permission, schema_version from musubi.storage_objects
                 where collection = $1 and key = $2 and owner_id = $3
                   and (expires_at is null or expires_at > now()) for update",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some((mut value, cur_version, cur_write, cur_sv)) = current else {
                return Err(Error::NotFound(format!(
                    "{}/{} does not exist",
                    op.collection, op.key
                )));
            };
            if caller != Caller::Server && cur_write == 0 {
                return Err(Error::PermissionDenied(format!(
                    "{}/{} is read-only",
                    op.collection, op.key
                )));
            }
            if matches!(op.version.as_deref(), Some(v) if v != cur_version) {
                return Err(Error::Conflict(format!(
                    "{}/{} version mismatch",
                    op.collection, op.key
                )));
            }
            if let Some(sc) = self.schema_of(&op.collection) {
                schema::migrate(&sc, &mut value, cur_sv);
            }
            merge_patch(&mut value, &op.patch);
            let schema_version = self.check_schema(&op.collection, &value)?;
            let changed_value = &value;
            let bytes = serde_json::to_vec(&value)?;
            if bytes.len() > MAX_VALUE_BYTES {
                return Err(Error::InvalidArgument("value exceeds 1 MiB".into()));
            }
            let new_version = version_of(&bytes);
            sqlx::query(
                "update musubi.storage_objects set value = $4, version = $5, schema_version = $6, updated_at = now()
                 where collection = $1 and key = $2 and owner_id = $3",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .bind(&value)
            .bind(&new_version)
            .bind(schema_version)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            changes.push(Change {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: Some(new_version.clone()),
                value: Some(changed_value.clone()),
                schema_version,
                deleted: false,
            });
            acks.push(Ack {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: new_version,
            });
        }
        self.enforce_quota(&mut tx, owner).await?;
        tx.commit().await.map_err(Error::internal)?;
        self.fire(changes);
        Ok(acks)
    }

    pub async fn read(&self, caller: Caller, refs: &[ReadRef]) -> Result<Vec<Object>> {
        if refs.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!(
                "at most {MAX_BATCH} objects per read"
            )));
        }
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            validate_name(&r.collection, "collection")?;
            validate_name(&r.key, "key")?;
            let owner = match (r.owner_id, caller) {
                (Some(o), _) => o,
                (None, Caller::User(a)) => a.0,
                (None, Caller::Server) => SYSTEM_OWNER,
            };
            let row = sqlx::query_as::<_, Row>(&format!(
                "select {COLS} from musubi.storage_objects where collection = $1 and key = $2 and owner_id = $3 and (expires_at is null or expires_at > now())"
            ))
            .bind(&r.collection)
            .bind(&r.key)
            .bind(owner)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
            if let Some(row) = row {
                let obj = to_object(row);
                if can_read(caller, &obj) {
                    out.push(obj);
                }
            }
        }
        Ok(self.finish(out))
    }

    pub async fn list(
        &self,
        caller: Caller,
        collection: &str,
        owner_id: Option<Uuid>,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<Object>, Option<String>)> {
        self.list_where(
            caller,
            collection,
            owner_id,
            limit,
            ListFilter {
                cursor,
                ..Default::default()
            },
        )
        .await
    }

    /// `list` with an optional JSONB containment filter: only objects whose `value` contains
    /// `filter` (`value @> filter`, served by the GIN index) are returned.
    pub async fn list_where(
        &self,
        caller: Caller,
        collection: &str,
        owner_id: Option<Uuid>,
        limit: i64,
        f: ListFilter<'_>,
    ) -> Result<(Vec<Object>, Option<String>)> {
        let ListFilter {
            cursor,
            filter,
            key_prefix,
            sort,
        } = f;
        if let Some(sort) = sort {
            return self
                .list_sorted(
                    caller, collection, owner_id, limit, cursor, filter, key_prefix, sort,
                )
                .await;
        }
        validate_name(collection, "collection")?;
        let (filter, ops) = schema::split_where(filter)?;
        let limit = limit.clamp(1, 100);
        let after_key = cursor.map(decode_cursor).transpose()?;
        let prefix_like = key_prefix.filter(|p| !p.is_empty()).map(|p| {
            format!(
                "{}%",
                p.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        // Users list their own objects by default; another owner's collection shows public objects only.
        let (owner_id, min_read): (Option<Uuid>, i16) = match (caller, owner_id) {
            (Caller::Server, o) => (o, 0),
            (Caller::User(a), None) => (Some(a.0), 1),
            (Caller::User(a), Some(o)) if o == a.0 => (Some(o), 1),
            (Caller::User(_), Some(o)) => (Some(o), 2),
        };
        let sql = format!(
            "select {COLS} from musubi.storage_objects
             where collection = $1 and ($2::uuid is null or owner_id = $2) and read_permission >= $3
               and (expires_at is null or expires_at > now())
               and ($4::text is null or (owner_id, key) > ($5::uuid, $4))
               and ($7::jsonb is null or value @> $7)
               and ($8::text is null or key like $8){}
             order by owner_id, key limit $6",
            schema::where_sql(&ops, 9)
        );
        let mut q = sqlx::query_as::<_, Row>(&sql)
            .bind(collection)
            .bind(owner_id)
            .bind(min_read)
            .bind(after_key.as_ref().map(|c| c.1.as_str()))
            .bind(after_key.as_ref().map(|c| c.0).unwrap_or(SYSTEM_OWNER))
            .bind(limit + 1)
            .bind(filter)
            .bind(prefix_like);
        for o in &ops {
            q = q.bind(&o.value);
        }
        let rows = q.fetch_all(&self.db).await.map_err(Error::internal)?;
        let mut objs: Vec<Object> = self.finish(rows.into_iter().map(to_object).collect());
        let next = if objs.len() as i64 > limit {
            objs.truncate(limit as usize);
            objs.last().map(|o| encode_cursor(o.owner_id, &o.key))
        } else {
            None
        };
        Ok((objs, next))
    }

    pub async fn write(
        &self,
        caller: Caller,
        owner_override: Option<Uuid>,
        ops: &[WriteOp],
    ) -> Result<Vec<Ack>> {
        if ops.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!(
                "at most {MAX_BATCH} objects per write"
            )));
        }
        let owner = match (caller, owner_override) {
            (Caller::Server, o) => o.unwrap_or(SYSTEM_OWNER),
            (Caller::User(a), Some(o)) if o != a.0 => {
                return Err(Error::PermissionDenied(
                    "cannot write objects owned by another user".into(),
                ))
            }
            (Caller::User(a), _) => a.0,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut acks = Vec::with_capacity(ops.len());
        let mut changes = Vec::new();
        for op in ops {
            validate_name(&op.collection, "collection")?;
            validate_name(&op.key, "key")?;
            if !op.value.is_object() {
                return Err(Error::InvalidArgument("value must be a JSON object".into()));
            }
            let bytes = serde_json::to_vec(&op.value)?;
            if bytes.len() > MAX_VALUE_BYTES {
                return Err(Error::InvalidArgument("value exceeds 1 MiB".into()));
            }
            if !(0..=2).contains(&op.read_permission) || !(0..=1).contains(&op.write_permission) {
                return Err(Error::InvalidArgument("invalid permission code".into()));
            }
            let new_version = version_of(&bytes);
            let schema_version = self.check_schema(&op.collection, &op.value)?;
            let changed_value = &op.value;

            let current = sqlx::query_as::<_, (String, i16)>(
                "select version, write_permission from musubi.storage_objects where collection = $1 and key = $2 and owner_id = $3 for update",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;

            if let Some((cur_version, cur_write)) = &current {
                if caller != Caller::Server && *cur_write == 0 {
                    return Err(Error::PermissionDenied(format!(
                        "{}/{} is read-only",
                        op.collection, op.key
                    )));
                }
                match op.version.as_deref() {
                    Some("*") => {
                        return Err(Error::Conflict(format!(
                            "{}/{} already exists",
                            op.collection, op.key
                        )))
                    }
                    Some(v) if v != cur_version => {
                        return Err(Error::Conflict(format!(
                            "{}/{} version mismatch",
                            op.collection, op.key
                        )))
                    }
                    _ => {}
                }
            } else if matches!(op.version.as_deref(), Some(v) if v != "*") {
                return Err(Error::Conflict(format!(
                    "{}/{} does not exist",
                    op.collection, op.key
                )));
            }

            let expires = op.ttl_secs.map(|t| {
                OffsetDateTime::now_utc()
                    + time::Duration::seconds(t.min(100 * 365 * 86_400) as i64)
            });
            sqlx::query(
                "insert into musubi.storage_objects (collection, key, owner_id, value, version, read_permission, write_permission, expires_at, schema_version)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 on conflict (collection, key, owner_id) do update set
                   value = excluded.value, version = excluded.version,
                   read_permission = excluded.read_permission, write_permission = excluded.write_permission,
                   expires_at = excluded.expires_at, schema_version = excluded.schema_version, updated_at = now()",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .bind(&op.value)
            .bind(&new_version)
            .bind(op.read_permission)
            .bind(op.write_permission)
            .bind(expires)
            .bind(schema_version)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            changes.push(Change {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: Some(new_version.clone()),
                value: Some(changed_value.clone()),
                schema_version,
                deleted: false,
            });
            acks.push(Ack {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: new_version,
            });
        }
        self.enforce_quota(&mut tx, owner).await?;
        tx.commit().await.map_err(Error::internal)?;
        self.fire(changes);
        Ok(acks)
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_sorted(
        &self,
        caller: Caller,
        collection: &str,
        owner_id: Option<Uuid>,
        limit: i64,
        cursor: Option<&str>,
        filter: Option<&serde_json::Value>,
        key_prefix: Option<&str>,
        sort: &str,
    ) -> Result<(Vec<Object>, Option<String>)> {
        validate_name(collection, "collection")?;
        let (filter, ops) = schema::split_where(filter)?;
        let (desc, field) = match sort.strip_prefix('-') {
            Some(f) => (true, f),
            None => (false, sort),
        };
        let path: Vec<String> = field.split('.').map(str::to_string).collect();
        if path.iter().any(|p| p.is_empty() || p.len() > 64) || path.len() > 8 {
            return Err(Error::InvalidArgument("bad sort field".into()));
        }
        let limit = limit.clamp(1, 100);
        let after = cursor.map(decode_sorted_cursor).transpose()?;
        let (owner_id, min_read) = visibility(caller, owner_id);
        let prefix_like = like_prefix(key_prefix);
        let (cmp, dir) = if desc { ("<", "desc") } else { (">", "asc") };
        let sql = format!(
            "select {COLS} from musubi.storage_objects
             where collection = $1 and ($2::uuid is null or owner_id = $2) and read_permission >= $3
               and (expires_at is null or expires_at > now())
               and ($4::jsonb is null or value @> $4)
               and ($5::text is null or key like $5)
               and ($7::jsonb is null or (coalesce(value #> $6, 'null'::jsonb), owner_id, key) {cmp} ($7, $8::uuid, $9::text)){}
             order by coalesce(value #> $6, 'null'::jsonb) {dir}, owner_id {dir}, key {dir} limit $10",
            schema::where_sql(&ops, 11)
        );
        let mut q = sqlx::query_as::<_, Row>(&sql)
            .bind(collection)
            .bind(owner_id)
            .bind(min_read)
            .bind(filter)
            .bind(prefix_like)
            .bind(&path)
            .bind(after.as_ref().map(|a| &a.0))
            .bind(after.as_ref().map(|a| a.1).unwrap_or(SYSTEM_OWNER))
            .bind(after.as_ref().map(|a| a.2.as_str()).unwrap_or(""))
            .bind(limit + 1);
        for o in &ops {
            q = q.bind(&o.value);
        }
        let rows = q.fetch_all(&self.db).await.map_err(Error::internal)?;
        let mut objs: Vec<Object> = self.finish(rows.into_iter().map(to_object).collect());
        let next = if objs.len() as i64 > limit {
            objs.truncate(limit as usize);
            objs.last().map(|o| {
                let sv = path
                    .iter()
                    .try_fold(&o.value, |v, k| v.get(k))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                encode_sorted_cursor(&sv, o.owner_id, &o.key)
            })
        } else {
            None
        };
        Ok((objs, next))
    }

    pub async fn delete(
        &self,
        caller: Caller,
        owner_override: Option<Uuid>,
        ops: &[DeleteOp],
    ) -> Result<()> {
        if ops.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!(
                "at most {MAX_BATCH} objects per delete"
            )));
        }
        let owner = match (caller, owner_override) {
            (Caller::Server, o) => o.unwrap_or(SYSTEM_OWNER),
            (Caller::User(a), Some(o)) if o != a.0 => {
                return Err(Error::PermissionDenied(
                    "cannot delete objects owned by another user".into(),
                ))
            }
            (Caller::User(a), _) => a.0,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut changes = Vec::new();
        for op in ops {
            validate_name(&op.collection, "collection")?;
            validate_name(&op.key, "key")?;
            let current = sqlx::query_as::<_, (String, i16)>(
                "select version, write_permission from musubi.storage_objects where collection = $1 and key = $2 and owner_id = $3 for update",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some((cur_version, cur_write)) = current else {
                continue;
            };
            if caller != Caller::Server && cur_write == 0 {
                return Err(Error::PermissionDenied(format!(
                    "{}/{} is read-only",
                    op.collection, op.key
                )));
            }
            if matches!(&op.version, Some(v) if *v != cur_version) {
                return Err(Error::Conflict(format!(
                    "{}/{} version mismatch",
                    op.collection, op.key
                )));
            }
            changes.push(Change {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: None,
                value: None,
                schema_version: 0,
                deleted: true,
            });
            sqlx::query("delete from musubi.storage_objects where collection = $1 and key = $2 and owner_id = $3")
                .bind(&op.collection)
                .bind(&op.key)
                .bind(owner)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        self.fire(changes);
        Ok(())
    }

    /// Schema/migration/index definitions as loaded at start (`load_schemas`) and kept in sync by
    /// `put_schema` / `delete_schema` on this node; other nodes pick changes up on their next load.
    pub fn schema_of(&self, collection: &str) -> Option<std::sync::Arc<CollectionSchema>> {
        self.schemas.read().ok()?.get(collection).cloned()
    }

    pub fn schemas(&self) -> Vec<CollectionSchema> {
        let mut v: Vec<CollectionSchema> = self
            .schemas
            .read()
            .map(|m| m.values().map(|s| (**s).clone()).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.collection.cmp(&b.collection));
        v
    }

    pub async fn load_schemas(&self) -> Result<usize> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                i32,
                serde_json::Value,
                serde_json::Value,
                serde_json::Value,
            ),
        >(
            "select collection, version, schema, migrations, indexes from musubi.storage_schemas",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut map = std::collections::HashMap::new();
        for (collection, version, schema, migrations, indexes) in rows {
            let sc = CollectionSchema {
                collection: collection.clone(),
                version,
                schema,
                migrations: serde_json::from_value(migrations).unwrap_or_default(),
                indexes: serde_json::from_value(indexes).unwrap_or_default(),
            };
            map.insert(collection, std::sync::Arc::new(sc));
        }
        let n = map.len();
        if let Ok(mut g) = self.schemas.write() {
            *g = map;
        }
        Ok(n)
    }

    /// Store a collection contract, (re)create its expression indexes and drop the ones removed.
    pub async fn put_schema(&self, s: CollectionSchema) -> Result<CollectionSchema> {
        validate_name(&s.collection, "collection")?;
        schema::validate_definition(&s)?;
        let before = self.schema_of(&s.collection);
        sqlx::query(
            "insert into musubi.storage_schemas (collection, version, schema, migrations, indexes)
             values ($1, $2, $3, $4, $5)
             on conflict (collection) do update set version = excluded.version, schema = excluded.schema,
               migrations = excluded.migrations, indexes = excluded.indexes, updated_at = now()",
        )
        .bind(&s.collection)
        .bind(s.version)
        .bind(&s.schema)
        .bind(serde_json::to_value(&s.migrations)?)
        .bind(serde_json::to_value(&s.indexes)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let keep: Vec<String> = s
            .indexes
            .iter()
            .map(|d| schema::index_name(&s.collection, d))
            .collect();
        for old in before.iter().flat_map(|b| b.indexes.iter()) {
            let name = schema::index_name(&s.collection, old);
            if !keep.contains(&name) {
                sqlx::query(&format!("drop index if exists musubi.{name}"))
                    .execute(&self.db)
                    .await
                    .map_err(Error::internal)?;
            }
        }
        for d in &s.indexes {
            sqlx::query(&schema::index_ddl(&s.collection, d)?)
                .execute(&self.db)
                .await
                .map_err(|e| match e {
                    sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => {
                        Error::Conflict(format!(
                            "unique index violated by existing objects: {}",
                            d.message()
                        ))
                    }
                    e => Error::internal(e),
                })?;
        }
        if let Ok(mut g) = self.schemas.write() {
            g.insert(s.collection.clone(), std::sync::Arc::new(s.clone()));
        }
        Ok(s)
    }

    pub async fn delete_schema(&self, collection: &str) -> Result<bool> {
        if let Some(old) = self.schema_of(collection) {
            for d in &old.indexes {
                sqlx::query(&format!(
                    "drop index if exists musubi.{}",
                    schema::index_name(collection, d)
                ))
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
            }
        }
        let n = sqlx::query("delete from musubi.storage_schemas where collection = $1")
            .bind(collection)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if let Ok(mut g) = self.schemas.write() {
            g.remove(collection);
        }
        Ok(n > 0)
    }

    /// Rewrite up to `batch` objects still at an older schema version; returns (migrated, remaining).
    pub async fn migrate_collection(&self, collection: &str, batch: i64) -> Result<(i64, i64)> {
        let Some(sc) = self.schema_of(collection) else {
            return Err(Error::NotFound(format!("no schema for {collection}")));
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows = sqlx::query_as::<_, (String, Uuid, serde_json::Value, i32)>(
            "select key, owner_id, value, schema_version from musubi.storage_objects
             where collection = $1 and schema_version < $2
             order by owner_id, key limit $3 for update skip locked",
        )
        .bind(collection)
        .bind(sc.version)
        .bind(batch.clamp(1, 5000))
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut migrated = 0i64;
        for (key, owner, mut value, sv) in rows {
            let reached = schema::migrate(&sc, &mut value, sv);
            if reached == sv {
                continue;
            }
            let bytes = serde_json::to_vec(&value)?;
            sqlx::query(
                "update musubi.storage_objects set value = $4, version = $5, schema_version = $6, updated_at = now()
                 where collection = $1 and key = $2 and owner_id = $3",
            )
            .bind(collection)
            .bind(&key)
            .bind(owner)
            .bind(&value)
            .bind(version_of(&bytes))
            .bind(reached)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            migrated += 1;
        }
        tx.commit().await.map_err(Error::internal)?;
        let remaining = sqlx::query_scalar::<_, i64>(
            "select count(*) from musubi.storage_objects where collection = $1 and schema_version < $2",
        )
        .bind(collection)
        .bind(sc.version)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok((migrated, remaining))
    }

    pub fn set_on_change(&self, f: impl Fn(Change) + Send + Sync + 'static) {
        if let Ok(mut g) = self.on_change.write() {
            *g = Some(std::sync::Arc::new(f));
        }
    }

    fn fire(&self, changes: Vec<Change>) {
        let hook = self.on_change.read().ok().and_then(|g| g.clone());
        if let Some(h) = hook {
            for c in changes {
                h(c);
            }
        }
    }

    fn check_schema(&self, collection: &str, value: &serde_json::Value) -> Result<i32> {
        match self.schema_of(collection) {
            Some(sc) => {
                if !sc.schema.is_null() {
                    schema::validate(&sc.schema, value)?;
                }
                Ok(sc.version)
            }
            None => Ok(0),
        }
    }

    /// Lazily migrate objects written under an older schema version.
    fn finish(&self, mut objs: Vec<Object>) -> Vec<Object> {
        for o in &mut objs {
            if let Some(sc) = self.schema_of(&o.collection) {
                if o.schema_version < sc.version {
                    o.schema_version = schema::migrate(&sc, &mut o.value, o.schema_version);
                }
            }
        }
        objs
    }
}

fn can_read(caller: Caller, o: &Object) -> bool {
    match caller {
        Caller::Server => true,
        Caller::User(a) => o.read_permission == 2 || (o.read_permission == 1 && o.owner_id == a.0),
    }
}

fn to_object(r: Row) -> Object {
    Object {
        collection: r.0,
        key: r.1,
        owner_id: r.2,
        value: r.3,
        version: r.4,
        read_permission: r.5,
        write_permission: r.6,
        created_at: r.7,
        updated_at: r.8,
        expires_at: r.9,
        schema_version: r.10,
    }
}

pub fn version_of(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes)[..16])
}

fn validate_name(s: &str, what: &str) -> Result<()> {
    if s.is_empty() || s.len() > 128 || s.chars().any(|c| c.is_control()) {
        return Err(Error::InvalidArgument(format!(
            "{what} must be 1..=128 printable chars"
        )));
    }
    Ok(())
}

fn encode_cursor(owner: Uuid, key: &str) -> String {
    format!("{}:{}", owner.simple(), hex::encode(key))
}

fn decode_cursor(c: &str) -> Result<(Uuid, String)> {
    let (o, k) = c
        .split_once(':')
        .ok_or_else(|| Error::InvalidArgument("bad cursor".into()))?;
    let owner = Uuid::parse_str(o).map_err(|_| Error::InvalidArgument("bad cursor".into()))?;
    let key = hex::decode(k)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| Error::InvalidArgument("bad cursor".into()))?;
    Ok((owner, key))
}

fn one() -> i16 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        let o = Uuid::now_v7();
        let c = encode_cursor(o, "some/key:with:colons");
        assert_eq!(
            decode_cursor(&c).unwrap(),
            (o, "some/key:with:colons".to_string())
        );
    }

    #[test]
    fn version_is_content_hash() {
        assert_eq!(version_of(b"{}"), version_of(b"{}"));
        assert_ne!(version_of(b"{}"), version_of(b"{\"a\":1}"));
        assert_eq!(version_of(b"x").len(), 32);
    }
}
