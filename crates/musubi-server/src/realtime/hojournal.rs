//! Handover journal: every step an entity takes while changing owner — local start, shipment to
//! another node, what the receiver did with it, the ack, the timeout or re-shipment, the end —
//! as one durable record per step. Metrics count these paths; the journal names the entity, so
//! a lost entity can be traced after the fact (`GET /v1/admin/worlds/{w}/handovers?entity=`)
//! instead of being caught live. Steps go into a bounded in-memory ring (the fresh tail, also
//! served by the endpoint) and are flushed to `musubi.handover_journal` in batches once a second.

use parking_lot::Mutex;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::VecDeque;
use std::sync::OnceLock;
use uuid::Uuid;

use musubi_core::Result;

/// Steps kept in memory per node; the oldest fall off (they are in Postgres by then unless the
/// node has no database).
pub const RING: usize = 65_536;

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub ms: i64,
    pub world: String,
    pub entity: Uuid,
    pub node: Uuid,
    pub tick: u64,
    /// `started` | `shipped` | `received` | `ack` | `ended`.
    pub event: &'static str,
    /// The branch taken, same names as the `path` label of the handover metrics.
    pub path: &'static str,
    pub detail: String,
}

struct Ring {
    node: Uuid,
    steps: VecDeque<Step>,
    unflushed: usize,
}

static RING_CELL: OnceLock<Mutex<Ring>> = OnceLock::new();

fn ring() -> &'static Mutex<Ring> {
    RING_CELL.get_or_init(|| {
        Mutex::new(Ring {
            node: Uuid::nil(),
            steps: VecDeque::with_capacity(1024),
            unflushed: 0,
        })
    })
}

/// Which node the steps recorded in this process belong to.
pub fn init(node: Uuid) {
    ring().lock().node = node;
}

pub fn record(
    world: &str,
    entity: Uuid,
    tick: u64,
    event: &'static str,
    path: &'static str,
    detail: impl Into<String>,
) {
    let mut r = ring().lock();
    let node = r.node;
    if r.steps.len() >= RING {
        r.steps.pop_front();
        r.unflushed = r.unflushed.min(r.steps.len());
    }
    r.steps.push_back(Step {
        ms: musubi_cluster::unix_ms() as i64,
        world: world.to_string(),
        entity,
        node,
        tick,
        event,
        path,
        detail: detail.into(),
    });
    r.unflushed += 1;
}

/// Steps still in memory for a world (and entity), newest last.
pub fn recent(world: &str, entity: Option<Uuid>, limit: usize) -> Vec<Step> {
    let r = ring().lock();
    let mut v: Vec<Step> = r
        .steps
        .iter()
        .rev()
        .filter(|s| s.world == world && entity.is_none_or(|e| s.entity == e))
        .take(limit)
        .cloned()
        .collect();
    v.reverse();
    v
}

fn take_unflushed() -> Vec<Step> {
    let mut r = ring().lock();
    let n = r.unflushed.min(r.steps.len());
    r.unflushed = 0;
    let start = r.steps.len() - n;
    r.steps.range(start..).cloned().collect()
}

/// Write the steps recorded since the last flush; returns how many.
pub async fn flush(db: &PgPool) -> Result<usize> {
    let steps = take_unflushed();
    if steps.is_empty() {
        return Ok(0);
    }
    let n = steps.len();
    let (mut names, mut ents, mut nodes, mut ticks, mut ats, mut evs, mut paths, mut dets) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    for s in steps {
        names.push(s.world);
        ents.push(s.entity);
        nodes.push(s.node);
        ticks.push(s.tick as i64);
        ats.push(s.ms);
        evs.push(s.event.to_string());
        paths.push(s.path.to_string());
        dets.push(s.detail);
    }
    sqlx::query(
        "insert into musubi.handover_journal (name, entity, node_id, tick, at, event, path, detail)
         select * from unnest($1::text[], $2::uuid[], $3::uuid[], $4::bigint[], $5::bigint[], $6::text[], $7::text[], $8::text[])",
    )
    .bind(&names)
    .bind(&ents)
    .bind(&nodes)
    .bind(&ticks)
    .bind(&ats)
    .bind(&evs)
    .bind(&paths)
    .bind(&dets)
    .execute(db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(n)
}

/// Durable steps of a world (and entity) from every node, oldest first, at most `limit`.
pub async fn load(db: &PgPool, world: &str, entity: Option<Uuid>, limit: i64) -> Result<Vec<Step>> {
    let rows: Vec<(Uuid, Uuid, i64, i64, String, String, String)> = sqlx::query_as(
        "select entity, node_id, tick, at, event, path, detail from musubi.handover_journal
         where name = $1 and ($2::uuid is null or entity = $2)
         order by at desc, id desc limit $3",
    )
    .bind(world)
    .bind(entity)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let mut v: Vec<Step> = rows
        .into_iter()
        .map(|(entity, node, tick, ms, event, path, detail)| Step {
            ms,
            world: world.to_string(),
            entity,
            node,
            tick: tick.max(0) as u64,
            event: intern(&event),
            path: intern(&path),
            detail,
        })
        .collect();
    v.reverse();
    Ok(v)
}

/// Steps are stored as text; back in memory they are the same static names (`Step` keeps them
/// `&'static str` so recording costs no allocation). An unknown name is leaked once.
fn intern(s: &str) -> &'static str {
    static KNOWN: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    let k = KNOWN.get_or_init(|| Mutex::new(Vec::new()));
    let mut k = k.lock();
    if let Some(x) = k.iter().find(|x| **x == s) {
        return x;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    k.push(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_order_and_bounds() {
        let w = format!("w-{}", Uuid::now_v7());
        let e = Uuid::now_v7();
        record(&w, e, 1, "started", "local", "a");
        record(&w, e, 2, "ended", "completed", "b");
        record(&w, Uuid::now_v7(), 3, "started", "local", "other");
        let v = recent(&w, Some(e), 10);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].event, v[1].path), ("started", "completed"));
        assert_eq!(recent(&w, None, 1).len(), 1);
        assert_eq!(recent(&w, None, 1)[0].detail, "other");
    }
}
