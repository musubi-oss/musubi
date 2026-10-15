//! In-memory Raft log (adapted from openraft's memstore example). Durable truth stays in Postgres.
#![allow(clippy::result_large_err)]

//! Provide `LogStore`, which is a in-memory implementation of `RaftLogStore` for demonstration
//! purpose only.

use redb::ReadableTable;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::ops::RangeBounds;
use std::path::PathBuf;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use openraft::storage::LogFlushed;
use openraft::LogId;
use openraft::LogState;
use openraft::RaftLogId;
use openraft::RaftTypeConfig;
use openraft::StorageError;
use openraft::Vote;
use tokio::sync::Mutex;

/// RaftLogStore implementation with a in-memory storage
#[derive(Clone, Debug, Default)]
pub struct LogStore<C: RaftTypeConfig> {
    inner: Arc<Mutex<LogStoreInner<C>>>,
}

#[derive(Debug)]
pub struct LogStoreInner<C: RaftTypeConfig> {
    /// The last purged log id.
    last_purged_log_id: Option<LogId<C::NodeId>>,

    /// The Raft log.
    log: BTreeMap<u64, C::Entry>,

    /// The commit log id.
    committed: Option<LogId<C::NodeId>>,

    /// The current granted vote.
    vote: Option<Vote<C::NodeId>>,
    /// Durable copy (`log.redb`: `log` table index → entry JSON, `meta` table vote/committed/purged);
    /// `None` = memory only.
    db: Option<redb::Database>,
}

const LOG: redb::TableDefinition<u64, &[u8]> = redb::TableDefinition::new("log");
const META: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("meta");

fn rdb(e: impl Into<redb::Error>) -> std::io::Error {
    std::io::Error::other(e.into())
}

fn bad(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
struct Meta<N: openraft::NodeId> {
    vote: Option<Vote<N>>,
    committed: Option<LogId<N>>,
    purged: Option<LogId<N>>,
}

impl<C: RaftTypeConfig> LogStore<C>
where
    C::Entry: Serialize + DeserializeOwned,
{
    /// Open a store rooted at `dir` (created if missing), replaying what a previous process left
    /// there. `None` gives the in-memory store.
    pub fn open(dir: Option<PathBuf>) -> std::io::Result<Self> {
        let mut inner = LogStoreInner::<C>::default();
        if let Some(dir) = dir {
            std::fs::create_dir_all(&dir)?;
            let db = redb::Database::create(dir.join("log.redb")).map_err(rdb)?;
            {
                let w = db.begin_write().map_err(rdb)?;
                w.open_table(LOG).map_err(rdb)?;
                w.open_table(META).map_err(rdb)?;
                w.commit().map_err(rdb)?;
            }
            let r = db.begin_read().map_err(rdb)?;
            {
                let meta = r.open_table(META).map_err(rdb)?;
                if let Some(m) = meta.get("meta").map_err(rdb)? {
                    let m: Meta<C::NodeId> = serde_json::from_slice(m.value()).map_err(bad)?;
                    inner.vote = m.vote;
                    inner.committed = m.committed;
                    inner.last_purged_log_id = m.purged;
                }
                let log = r.open_table(LOG).map_err(rdb)?;
                for item in log.iter().map_err(rdb)? {
                    let (k, v) = item.map_err(rdb)?;
                    let e: C::Entry = serde_json::from_slice(v.value()).map_err(bad)?;
                    inner.log.insert(k.value(), e);
                }
            }
            drop(r);
            inner.db = Some(db);
            inner.import_legacy(&dir)?;
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }
}

impl<C: RaftTypeConfig> LogStoreInner<C>
where
    C::Entry: Serialize,
{
    fn io_err(e: std::io::Error) -> StorageError<C::NodeId> {
        openraft::StorageIOError::write_logs(&e).into()
    }

    /// One-time import of the pre-redb layout (`meta.json` + `log.jsonl`) into an empty database;
    /// the old files are renamed `*.migrated` afterwards.
    fn import_legacy(&mut self, dir: &std::path::Path) -> std::io::Result<()>
    where
        C::Entry: DeserializeOwned,
    {
        let (meta_p, log_p) = (dir.join("meta.json"), dir.join("log.jsonl"));
        if !meta_p.exists() && !log_p.exists() {
            return Ok(());
        }
        if self.log.is_empty() && self.vote.is_none() {
            if let Ok(bytes) = std::fs::read(&meta_p) {
                let m: Meta<C::NodeId> = serde_json::from_slice(&bytes).map_err(bad)?;
                self.vote = m.vote;
                self.committed = m.committed;
                self.last_purged_log_id = m.purged;
            }
            if let Ok(text) = std::fs::read_to_string(&log_p) {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let e: C::Entry = serde_json::from_str(line).map_err(bad)?;
                    self.log.insert(e.get_log_id().index, e);
                }
            }
            let all: Vec<&C::Entry> = self.log.values().collect();
            self.append_disk(&all).map_err(std::io::Error::other)?;
            self.persist_meta().map_err(std::io::Error::other)?;
            tracing::info!(
                entries = all.len(),
                "raft log imported from jsonl into redb"
            );
        }
        for p in [meta_p, log_p] {
            if p.exists() {
                let mut moved = p.clone().into_os_string();
                moved.push(".migrated");
                let _ = std::fs::rename(&p, moved);
            }
        }
        Ok(())
    }

    fn persist_meta(&self) -> Result<(), StorageError<C::NodeId>> {
        let Some(db) = &self.db else { return Ok(()) };
        let m = Meta::<C::NodeId> {
            vote: self.vote.clone(),
            committed: self.committed.clone(),
            purged: self.last_purged_log_id.clone(),
        };
        let bytes = serde_json::to_vec(&m).map_err(|e| Self::io_err(bad(e)))?;
        let w = db.begin_write().map_err(|e| Self::io_err(rdb(e)))?;
        w.open_table(META)
            .map_err(|e| Self::io_err(rdb(e)))?
            .insert("meta", bytes.as_slice())
            .map_err(|e| Self::io_err(rdb(e)))?;
        w.commit().map_err(|e| Self::io_err(rdb(e)))
    }

    fn append_disk(&self, entries: &[&C::Entry]) -> Result<(), StorageError<C::NodeId>> {
        let Some(db) = &self.db else { return Ok(()) };
        let w = db.begin_write().map_err(|e| Self::io_err(rdb(e)))?;
        {
            let mut t = w.open_table(LOG).map_err(|e| Self::io_err(rdb(e)))?;
            for e in entries {
                let bytes = serde_json::to_vec(e).map_err(|e| Self::io_err(bad(e)))?;
                t.insert(e.get_log_id().index, bytes.as_slice())
                    .map_err(|e| Self::io_err(rdb(e)))?;
            }
        }
        w.commit().map_err(|e| Self::io_err(rdb(e)))
    }

    /// Delete a range of indexes on disk (truncate = `from..`, purge = `..=upto`): one
    /// transaction, no rewrite of the whole log.
    fn remove_disk(
        &self,
        range: impl std::ops::RangeBounds<u64>,
    ) -> Result<(), StorageError<C::NodeId>> {
        let Some(db) = &self.db else { return Ok(()) };
        let w = db.begin_write().map_err(|e| Self::io_err(rdb(e)))?;
        {
            let mut t = w.open_table(LOG).map_err(|e| Self::io_err(rdb(e)))?;
            let drained = t
                .extract_from_if(range, |_, _| true)
                .map_err(|e| Self::io_err(rdb(e)))?;
            for item in drained {
                item.map_err(|e| Self::io_err(rdb(e)))?;
            }
        }
        w.commit().map_err(|e| Self::io_err(rdb(e)))
    }
}

impl<C: RaftTypeConfig> Default for LogStoreInner<C> {
    fn default() -> Self {
        Self {
            last_purged_log_id: None,
            log: BTreeMap::new(),
            committed: None,
            vote: None,
            db: None,
        }
    }
}

impl<C: RaftTypeConfig> LogStoreInner<C>
where
    C::Entry: Serialize,
{
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug>(
        &mut self,
        range: RB,
    ) -> Result<Vec<C::Entry>, StorageError<C::NodeId>>
    where
        C::Entry: Clone,
    {
        let response = self
            .log
            .range(range.clone())
            .map(|(_, val)| val.clone())
            .collect::<Vec<_>>();
        Ok(response)
    }

    async fn get_log_state(&mut self) -> Result<LogState<C>, StorageError<C::NodeId>> {
        let last = self
            .log
            .iter()
            .next_back()
            .map(|(_, ent)| ent.get_log_id().clone());

        let last_purged = self.last_purged_log_id.clone();

        let last = match last {
            None => last_purged.clone(),
            Some(x) => Some(x),
        };

        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<C::NodeId>>,
    ) -> Result<(), StorageError<C::NodeId>> {
        self.committed = committed;
        self.persist_meta()
    }

    async fn read_committed(
        &mut self,
    ) -> Result<Option<LogId<C::NodeId>>, StorageError<C::NodeId>> {
        Ok(self.committed.clone())
    }

    async fn save_vote(&mut self, vote: &Vote<C::NodeId>) -> Result<(), StorageError<C::NodeId>> {
        self.vote = Some(vote.clone());
        self.persist_meta()
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<C::NodeId>>, StorageError<C::NodeId>> {
        Ok(self.vote.clone())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<C>,
    ) -> Result<(), StorageError<C::NodeId>>
    where
        I: IntoIterator<Item = C::Entry>,
    {
        // Simple implementation that calls the flush-before-return `append_to_log`.
        let mut idx = Vec::new();
        for entry in entries {
            let i = entry.get_log_id().index;
            self.log.insert(i, entry);
            idx.push(i);
        }
        let new: Vec<&C::Entry> = idx.iter().filter_map(|i| self.log.get(i)).collect();
        let r = self.append_disk(&new);
        callback.log_io_completed(
            r.as_ref()
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string())),
        );
        r
    }

    async fn truncate(&mut self, log_id: LogId<C::NodeId>) -> Result<(), StorageError<C::NodeId>> {
        let keys = self
            .log
            .range(log_id.index..)
            .map(|(k, _v)| *k)
            .collect::<Vec<_>>();
        for key in keys {
            self.log.remove(&key);
        }

        self.remove_disk(log_id.index..)
    }

    async fn purge(&mut self, log_id: LogId<C::NodeId>) -> Result<(), StorageError<C::NodeId>> {
        {
            let ld = &mut self.last_purged_log_id;
            assert!(ld.as_ref() <= Some(&log_id));
            *ld = Some(log_id.clone());
        }

        {
            let keys = self
                .log
                .range(..=log_id.index)
                .map(|(k, _v)| *k)
                .collect::<Vec<_>>();
            for key in keys {
                self.log.remove(&key);
            }
        }

        self.remove_disk(..=log_id.index)?;
        self.persist_meta()
    }
}

mod impl_log_store {
    use std::fmt::Debug;
    use std::ops::RangeBounds;

    use openraft::storage::LogFlushed;
    use openraft::storage::RaftLogStorage;
    use openraft::LogId;
    use openraft::LogState;
    use openraft::RaftLogReader;
    use openraft::RaftTypeConfig;
    use openraft::StorageError;
    use openraft::Vote;

    use super::LogStore;

    impl<C: RaftTypeConfig> RaftLogReader<C> for LogStore<C>
    where
        C::Entry: Clone + serde::Serialize,
    {
        async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug>(
            &mut self,
            range: RB,
        ) -> Result<Vec<C::Entry>, StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.try_get_log_entries(range).await
        }
    }

    impl<C: RaftTypeConfig> RaftLogStorage<C> for LogStore<C>
    where
        C::Entry: Clone + serde::Serialize,
    {
        type LogReader = Self;

        async fn get_log_state(&mut self) -> Result<LogState<C>, StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.get_log_state().await
        }

        async fn save_committed(
            &mut self,
            committed: Option<LogId<C::NodeId>>,
        ) -> Result<(), StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.save_committed(committed).await
        }

        async fn read_committed(
            &mut self,
        ) -> Result<Option<LogId<C::NodeId>>, StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.read_committed().await
        }

        async fn save_vote(
            &mut self,
            vote: &Vote<C::NodeId>,
        ) -> Result<(), StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.save_vote(vote).await
        }

        async fn read_vote(&mut self) -> Result<Option<Vote<C::NodeId>>, StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.read_vote().await
        }

        async fn append<I>(
            &mut self,
            entries: I,
            callback: LogFlushed<C>,
        ) -> Result<(), StorageError<C::NodeId>>
        where
            I: IntoIterator<Item = C::Entry>,
        {
            let mut inner = self.inner.lock().await;
            inner.append(entries, callback).await
        }

        async fn truncate(
            &mut self,
            log_id: LogId<C::NodeId>,
        ) -> Result<(), StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.truncate(log_id).await
        }

        async fn purge(&mut self, log_id: LogId<C::NodeId>) -> Result<(), StorageError<C::NodeId>> {
            let mut inner = self.inner.lock().await;
            inner.purge(log_id).await
        }

        async fn get_log_reader(&mut self) -> Self::LogReader {
            self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::{DirectoryOp, TypeConfig};
    use openraft::storage::RaftLogStorage;
    use openraft::testing::log_id;
    use openraft::{Entry, EntryPayload, RaftLogReader};

    #[tokio::test]
    async fn log_store_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("musubi-raft-{}", uuid::Uuid::now_v7()));
        {
            let store = LogStore::<TypeConfig>::open(Some(dir.clone())).unwrap();
            let mut inner = store.inner.lock().await;
            inner.save_vote(&Vote::new(3, 7)).await.unwrap();
            for i in 1..=5u64 {
                let e = Entry {
                    log_id: log_id(3, 7, i),
                    payload: EntryPayload::Normal(DirectoryOp::Set {
                        key: format!("k{i}"),
                        value: serde_json::json!(i),
                    }),
                };
                inner.log.insert(i, e);
            }
            let all: Vec<&Entry<TypeConfig>> = inner.log.values().collect();
            inner.append_disk(&all).unwrap();
            inner.truncate(log_id(3, 7, 5)).await.unwrap();
            inner.purge(log_id(3, 7, 1)).await.unwrap();
            inner.save_committed(Some(log_id(3, 7, 4))).await.unwrap();
        }
        let mut store = LogStore::<TypeConfig>::open(Some(dir.clone())).unwrap();
        assert_eq!(store.read_vote().await.unwrap(), Some(Vote::new(3, 7)));
        assert_eq!(store.read_committed().await.unwrap(), Some(log_id(3, 7, 4)));
        let st = store.get_log_state().await.unwrap();
        assert_eq!(st.last_purged_log_id, Some(log_id(3, 7, 1)));
        assert_eq!(st.last_log_id, Some(log_id(3, 7, 4)));
        let got = store.try_get_log_entries(2..=4).await.unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].log_id.index, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A data_dir written by the JSONL layout is imported into redb on first open and the old
    /// files are set aside.
    #[tokio::test]
    async fn legacy_jsonl_layout_is_imported() {
        let dir = std::env::temp_dir().join(format!("musubi-raft-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = Meta::<u64> {
            vote: Some(Vote::new(2, 9)),
            committed: Some(log_id(2, 9, 2)),
            purged: None,
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        let mut lines = String::new();
        for i in 1..=3u64 {
            let e: Entry<TypeConfig> = Entry {
                log_id: log_id(2, 9, i),
                payload: EntryPayload::Normal(DirectoryOp::Set {
                    key: format!("k{i}"),
                    value: serde_json::json!(i),
                }),
            };
            lines.push_str(&serde_json::to_string(&e).unwrap());
            lines.push('\n');
        }
        std::fs::write(dir.join("log.jsonl"), lines).unwrap();

        let mut store = LogStore::<TypeConfig>::open(Some(dir.clone())).unwrap();
        assert_eq!(store.read_vote().await.unwrap(), Some(Vote::new(2, 9)));
        assert_eq!(
            store.get_log_state().await.unwrap().last_log_id,
            Some(log_id(2, 9, 3))
        );
        assert!(dir.join("log.redb").exists());
        assert!(!dir.join("log.jsonl").exists() && dir.join("log.jsonl.migrated").exists());
        drop(store);
        // second open reads redb only
        let mut store = LogStore::<TypeConfig>::open(Some(dir.clone())).unwrap();
        assert_eq!(store.try_get_log_entries(1..=3).await.unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
