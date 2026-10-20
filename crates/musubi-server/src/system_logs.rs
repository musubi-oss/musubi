//! In-memory ring of the node's own log lines (Pragma `SystemReporter.GetLogsV1` parity): a
//! `tracing` layer keeps the last `[log].ring_lines` events so operators can tail the API
//! process without shell access; `GET /v1/admin/logs` reads it (and other nodes' rings over the bus).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

pub const DEFAULT_CAPACITY: usize = 5000;
pub const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub seq: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

struct Ring {
    cap: usize,
    seq: AtomicU64,
    lines: Mutex<VecDeque<Line>>,
}

static RING: OnceLock<Ring> = OnceLock::new();

fn ring_with(cap: usize) -> &'static Ring {
    RING.get_or_init(|| Ring {
        cap: cap.max(16),
        seq: AtomicU64::new(0),
        lines: Mutex::new(VecDeque::with_capacity(cap.clamp(16, 4096))),
    })
}

fn ring() -> &'static Ring {
    ring_with(DEFAULT_CAPACITY)
}

/// The layer to add to the subscriber; `capacity` fixes the ring size on first use.
pub fn layer(capacity: usize) -> RingLayer {
    ring_with(capacity);
    RingLayer
}

pub fn capacity() -> usize {
    ring().cap
}

pub fn push(line: Line) {
    let r = ring();
    let mut g = r.lines.lock().unwrap_or_else(|p| p.into_inner());
    if g.len() >= r.cap {
        g.pop_front();
    }
    g.push_back(line);
}

#[derive(Clone, Copy)]
pub struct RingLayer;

struct FieldCollector {
    message: String,
    fields: Map<String, Value>,
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .insert(field.name().into(), Value::String(format!("{value:?}")));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.into();
        } else {
            self.fields.insert(field.name().into(), value.into());
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().into(), value.into());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .insert(field.name().into(), Value::String(value.to_string()));
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut c = FieldCollector {
            message: String::new(),
            fields: Map::new(),
        };
        event.record(&mut c);
        let md = event.metadata();
        let r = ring();
        push(Line {
            seq: r.seq.fetch_add(1, Ordering::Relaxed) + 1,
            at: OffsetDateTime::now_utc(),
            level: md.level().as_str().to_ascii_lowercase(),
            target: md.target().to_string(),
            message: c.message,
            fields: c.fields,
        });
    }
}

pub fn level_rank(level: &str) -> Option<u8> {
    Some(match level.to_ascii_lowercase().as_str() {
        "error" => 0,
        "warn" | "warning" => 1,
        "info" => 2,
        "debug" => 3,
        "trace" => 4,
        _ => return None,
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    /// Minimum severity to include (`error` < `warn` < `info` < `debug` < `trace`).
    pub level: Option<String>,
    /// Target prefix (`musubi_fleet`, `musubi_server::http`).
    pub target: Option<String>,
    /// Case-insensitive substring of the message or any field.
    pub q: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    /// Only lines with `seq` below this (paging backwards).
    pub before: Option<u64>,
    pub limit: Option<usize>,
}

pub fn validate(q: &Query) -> Result<u8, String> {
    if let Some(l) = q.limit {
        if l == 0 || l > MAX_LIMIT {
            return Err(format!("limit must be 1..={MAX_LIMIT}"));
        }
    }
    match q.level.as_deref() {
        None => Ok(4),
        Some(l) => level_rank(l).ok_or_else(|| format!("unknown level {l:?}")),
    }
}

/// Newest-first lines matching `q` (`limit` default 200).
pub fn query(q: &Query) -> Result<Vec<Line>, String> {
    let max_rank = validate(q)?;
    let limit = q.limit.unwrap_or(200);
    let needle = q.q.as_deref().map(|s| s.to_ascii_lowercase());
    let g = ring().lines.lock().unwrap_or_else(|p| p.into_inner());
    let out = g
        .iter()
        .rev()
        .filter(|l| level_rank(&l.level).unwrap_or(4) <= max_rank)
        .filter(|l| q.target.as_deref().is_none_or(|t| l.target.starts_with(t)))
        .filter(|l| q.since.is_none_or(|s| l.at >= s))
        .filter(|l| q.before.is_none_or(|b| l.seq < b))
        .filter(|l| {
            needle.as_deref().is_none_or(|n| {
                l.message.to_ascii_lowercase().contains(n)
                    || l.fields
                        .values()
                        .any(|v| v.to_string().to_ascii_lowercase().contains(n))
            })
        })
        .take(limit)
        .cloned()
        .collect();
    Ok(out)
}

pub fn stats() -> (usize, u64) {
    let r = ring();
    let g = r.lines.lock().unwrap_or_else(|p| p.into_inner());
    (g.len(), r.seq.load(Ordering::Relaxed))
}
