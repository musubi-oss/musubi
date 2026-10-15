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

#[derive(Clone)]
pub struct StorageService {
    db: PgPool,
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
);

const COLS: &str = "collection, key, owner_id, value, version, read_permission, write_permission, created_at, updated_at, expires_at";

impl StorageService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
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
        Ok(out)
    }

    pub async fn list(
        &self,
        caller: Caller,
        collection: &str,
        owner_id: Option<Uuid>,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<Object>, Option<String>)> {
        self.list_where(caller, collection, owner_id, limit, cursor, None)
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
        cursor: Option<&str>,
        filter: Option<&serde_json::Value>,
    ) -> Result<(Vec<Object>, Option<String>)> {
        validate_name(collection, "collection")?;
        if let Some(f) = filter {
            if !f.is_object() {
                return Err(Error::InvalidArgument("where must be a JSON object".into()));
            }
        }
        let limit = limit.clamp(1, 100);
        let after_key = cursor.map(decode_cursor).transpose()?;
        // Users list their own objects by default; another owner's collection shows public objects only.
        let (owner_id, min_read): (Option<Uuid>, i16) = match (caller, owner_id) {
            (Caller::Server, o) => (o, 0),
            (Caller::User(a), None) => (Some(a.0), 1),
            (Caller::User(a), Some(o)) if o == a.0 => (Some(o), 1),
            (Caller::User(_), Some(o)) => (Some(o), 2),
        };
        let rows = sqlx::query_as::<_, Row>(&format!(
            "select {COLS} from musubi.storage_objects
             where collection = $1 and ($2::uuid is null or owner_id = $2) and read_permission >= $3
               and (expires_at is null or expires_at > now())
               and ($4::text is null or (owner_id, key) > ($5::uuid, $4))
               and ($7::jsonb is null or value @> $7)
             order by owner_id, key limit $6"
        ))
        .bind(collection)
        .bind(owner_id)
        .bind(min_read)
        .bind(after_key.as_ref().map(|c| c.1.as_str()))
        .bind(after_key.as_ref().map(|c| c.0).unwrap_or(SYSTEM_OWNER))
        .bind(limit + 1)
        .bind(filter)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut objs: Vec<Object> = rows.into_iter().map(to_object).collect();
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
                "insert into musubi.storage_objects (collection, key, owner_id, value, version, read_permission, write_permission, expires_at)
                 values ($1, $2, $3, $4, $5, $6, $7, $8)
                 on conflict (collection, key, owner_id) do update set
                   value = excluded.value, version = excluded.version,
                   read_permission = excluded.read_permission, write_permission = excluded.write_permission,
                   expires_at = excluded.expires_at, updated_at = now()",
            )
            .bind(&op.collection)
            .bind(&op.key)
            .bind(owner)
            .bind(&op.value)
            .bind(&new_version)
            .bind(op.read_permission)
            .bind(op.write_permission)
            .bind(expires)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            acks.push(Ack {
                collection: op.collection.clone(),
                key: op.key.clone(),
                owner_id: owner,
                version: new_version,
            });
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(acks)
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
            sqlx::query("delete from musubi.storage_objects where collection = $1 and key = $2 and owner_id = $3")
                .bind(&op.collection)
                .bind(&op.key)
                .bind(owner)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)
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
