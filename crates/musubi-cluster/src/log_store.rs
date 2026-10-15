//! In-memory Raft log (adapted from openraft's memstore example). Durable truth stays in Postgres.
#![allow(clippy::result_large_err)]

//! Provide `LogStore`, which is a in-memory implementation of `RaftLogStore` for demonstration
//! purpose only.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io::Write;
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
    /// Durable copy (`meta.json` + `log.jsonl`); `None` = memory only.
    dir: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
struct Meta<N: openraft::NodeId> {
    vote: Option<Vote<N>>,
    committed: Option<LogId<N>>,
    purged: Option<LogId<N>>,
}

fn write_atomic(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)
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
            if let Ok(bytes) = std::fs::read(dir.join("meta.json")) {
                let m: Meta<C::NodeId> = serde_json::from_slice(&bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                inner.vote = m.vote;
                inner.committed = m.committed;
                inner.last_purged_log_id = m.purged;
            }
            if let Ok(text) = std::fs::read_to_string(dir.join("log.jsonl")) {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let e: C::Entry = serde_json::from_str(line)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                    inner.log.insert(e.get_log_id().index, e);
                }
            }
            inner.dir = Some(dir);
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

    fn persist_meta(&self) -> Result<(), StorageError<C::NodeId>> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let m = Meta::<C::NodeId> {
            vote: self.vote.clone(),
            committed: self.committed.clone(),
            purged: self.last_purged_log_id.clone(),
        };
        let bytes = serde_json::to_vec(&m)
            .map_err(|e| Self::io_err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        write_atomic(&dir.join("meta.json"), &bytes).map_err(Self::io_err)
    }

    fn append_disk(&self, entries: &[&C::Entry]) -> Result<(), StorageError<C::NodeId>> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("log.jsonl"))
            .map_err(Self::io_err)?;
        for e in entries {
            let line = serde_json::to_vec(e).map_err(|e| {
                Self::io_err(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
            f.write_all(&line).map_err(Self::io_err)?;
            f.write_all(b"\n").map_err(Self::io_err)?;
        }
        f.sync_data().map_err(Self::io_err)
    }

    /// Rewrite `log.jsonl` from memory (after truncate/purge).
    fn rewrite_disk(&self) -> Result<(), StorageError<C::NodeId>> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let mut buf = Vec::new();
        for e in self.log.values() {
            serde_json::to_writer(&mut buf, e).map_err(|e| {
                Self::io_err(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
            buf.push(b'\n');
        }
        write_atomic(&dir.join("log.jsonl"), &buf).map_err(Self::io_err)
    }
}

impl<C: RaftTypeConfig> Default for LogStoreInner<C> {
    fn default() -> Self {
        Self {
            last_purged_log_id: None,
            log: BTreeMap::new(),
            committed: None,
            vote: None,
            dir: None,
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

        self.rewrite_disk()
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

        self.rewrite_disk()?;
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
}
