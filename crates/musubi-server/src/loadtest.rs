//! P1-35: simulated players driven by the server itself.
//!
//! `POST /v1/admin/dev/loadtests` spawns `bots` thin clients inside this node: each one
//! authenticates with a stable device id (HMAC of `loadbot:<n:06>` under the server key, so accounts do not pile up across runs),
//! opens a real WebSocket to the node (or another environment when `[dev.loadtest] allow_remote`)
//! and plays a scenario at `rate_hz` until the deadline or `…/stop`. Every request carries a
//! `cid`; the matching reply closes an RTT sample, so the numbers are what a client would see
//! through the whole stack (socket, hub, chat/match/RPC code, fan-out), not a synthetic timer.
//! The run keeps a per-second timeline (connected bots, sent/received, p50/p99 of that second)
//! and a final report with percentiles, throughput and errors by kind; finished runs are kept in
//! `musubi.loadtest_runs` so `…/{id}/compare/{base}` can flag a regression before a release.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use musubi_core::{Error, Result};
use musubi_proto::realtime::ChannelKind;
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::app::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub name: String,
    /// `ws://host:port/ws`; defaults to this node.
    pub url: Option<String>,
    /// `json` or `binary` (postcard).
    pub format: String,
    pub bots: u32,
    /// Bots are started evenly across this window (0 = all at once).
    pub ramp_secs: u64,
    /// Measured window after the ramp.
    pub duration_secs: u64,
    /// `ping` | `chat` | `match` | `rpc` | `custom` (`steps`).
    pub scenario: String,
    /// Requests per bot per second.
    pub rate_hz: f64,
    /// `chat`: room every bot joins and posts to.
    pub channel: String,
    /// `match`: players per relayed match (one creates, the rest join).
    pub match_size: u32,
    /// `match`: payload bytes per `MatchDataSend`.
    pub payload_bytes: u32,
    /// `rpc`: function id and payload.
    pub rpc_id: String,
    pub rpc_payload: Value,
    /// `custom`: steps executed in a loop, one per tick.
    pub steps: Vec<Step>,
    /// Unanswered requests per bot before new ones count as `timeout` and are not sent.
    pub max_in_flight: u32,
    /// A request without a reply after this long is a `timeout` sample.
    pub reply_timeout_ms: u64,
    pub tags: Value,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: None,
            format: "json".into(),
            bots: 10,
            ramp_secs: 0,
            duration_secs: 10,
            scenario: "ping".into(),
            rate_hz: 5.0,
            channel: "loadtest".into(),
            match_size: 4,
            payload_bytes: 64,
            rpc_id: String::new(),
            rpc_payload: Value::Null,
            steps: Vec::new(),
            max_in_flight: 32,
            reply_timeout_ms: 5000,
            tags: json!({}),
        }
    }
}

/// One tick of a `custom` scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    Ping,
    Rpc {
        id: String,
        #[serde(default)]
        payload: Value,
    },
    ChatSend {
        #[serde(default)]
        content: Value,
    },
    Status {
        #[serde(default)]
        status: Option<String>,
    },
    /// Pause this bot for `ms` instead of sending.
    Sleep {
        ms: u64,
    },
}

impl Request {
    pub fn validate(&self, cfg: &musubi_core::config::LoadtestConfig) -> Result<()> {
        if self.name.len() > 64 {
            return Err(Error::InvalidArgument("name: max 64 chars".into()));
        }
        if !matches!(self.format.as_str(), "json" | "binary") {
            return Err(Error::InvalidArgument(
                "format must be json or binary".into(),
            ));
        }
        if self.bots == 0 || self.bots > cfg.max_bots {
            return Err(Error::InvalidArgument(format!(
                "bots must be 1..={}",
                cfg.max_bots
            )));
        }
        if self.duration_secs == 0 || self.duration_secs > cfg.max_duration_secs {
            return Err(Error::InvalidArgument(format!(
                "duration_secs must be 1..={}",
                cfg.max_duration_secs
            )));
        }
        if self.ramp_secs > self.duration_secs.max(60) * 10 {
            return Err(Error::InvalidArgument("ramp_secs too long".into()));
        }
        if !(0.1..=200.0).contains(&self.rate_hz) || self.rate_hz.is_nan() {
            return Err(Error::InvalidArgument("rate_hz must be 0.1..=200".into()));
        }
        if self.max_in_flight == 0 || self.max_in_flight > 1024 {
            return Err(Error::InvalidArgument(
                "max_in_flight must be 1..=1024".into(),
            ));
        }
        if !(100..=60_000).contains(&self.reply_timeout_ms) {
            return Err(Error::InvalidArgument(
                "reply_timeout_ms must be 100..=60000".into(),
            ));
        }
        match self.scenario.as_str() {
            "ping" => {}
            "chat" => {
                if self.channel.is_empty() || self.channel.len() > 64 {
                    return Err(Error::InvalidArgument("channel: 1..=64 chars".into()));
                }
            }
            "match" => {
                if !(1..=64).contains(&self.match_size) {
                    return Err(Error::InvalidArgument("match_size must be 1..=64".into()));
                }
                if self.payload_bytes < 8 || self.payload_bytes > 16_384 {
                    return Err(Error::InvalidArgument(
                        "payload_bytes must be 8..=16384".into(),
                    ));
                }
            }
            "rpc" => {
                if self.rpc_id.is_empty() {
                    return Err(Error::InvalidArgument(
                        "rpc_id required for rpc scenario".into(),
                    ));
                }
            }
            "custom" => {
                if self.steps.is_empty() || self.steps.len() > 64 {
                    return Err(Error::InvalidArgument(
                        "steps: 1..=64 for custom scenario".into(),
                    ));
                }
                for st in &self.steps {
                    match st {
                        Step::Rpc { id, .. } if id.is_empty() => {
                            return Err(Error::InvalidArgument("steps: rpc id required".into()))
                        }
                        Step::Sleep { ms } if *ms == 0 || *ms > 60_000 => {
                            return Err(Error::InvalidArgument("steps: sleep ms 1..=60000".into()))
                        }
                        _ => {}
                    }
                }
            }
            other => {
                return Err(Error::InvalidArgument(format!(
                    "scenario {other}: ping, chat, match, rpc or custom"
                )))
            }
        }
        if let Some(u) = &self.url {
            let parsed = reqwest::Url::parse(u)
                .map_err(|_| Error::InvalidArgument("url: not a valid URL".into()))?;
            if !matches!(parsed.scheme(), "ws" | "wss") {
                return Err(Error::InvalidArgument(
                    "url must start with ws:// or wss://".into(),
                ));
            }
            let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if !local && !cfg.allow_remote {
                return Err(Error::PermissionDenied(
                    "url: remote targets need [dev.loadtest] allow_remote".into(),
                ));
            }
        }
        Ok(())
    }
}

/// A sample: `kind` is the request kind (ping/chat/match/rpc/status/connect/auth) or an error
/// class (connect_error/auth_error/send_error/closed/timeout/server_error).
struct Sample {
    kind: &'static str,
    ms: f64,
    ok: bool,
}

struct Live {
    id: Uuid,
    stop: watch::Sender<bool>,
    snapshot: Mutex<Value>,
    connected: AtomicU64,
    active: AtomicU64,
    sent: AtomicU64,
    received: AtomicU64,
    stopped: AtomicBool,
}

static ACTIVE: OnceLock<Mutex<Option<Arc<Live>>>> = OnceLock::new();

fn active() -> &'static Mutex<Option<Arc<Live>>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}

pub async fn start(s: &AppState, req: Request) -> Result<Value> {
    let cfg = &s.cfg.dev.loadtest;
    req.validate(cfg)?;
    let id = Uuid::now_v7();
    {
        let mut a = active().lock().unwrap();
        if let Some(l) = a.as_ref() {
            return Err(Error::Conflict(format!(
                "run {} is still active on this node",
                l.id
            )));
        }
        let (stop, _) = watch::channel(false);
        *a = Some(Arc::new(Live {
            id,
            stop,
            snapshot: Mutex::new(json!({})),
            connected: AtomicU64::new(0),
            active: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        }));
    }
    let node = s.cfg.http.listen.to_string();
    let url = req
        .url
        .clone()
        .unwrap_or_else(|| format!("ws://127.0.0.1:{}/ws", s.cfg.http.listen.port()));
    sqlx::query(
        "insert into musubi.loadtest_runs (id, name, state, node, request, started_at) values ($1, $2, 'running', $3, $4, now())",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&node)
    .bind(serde_json::to_value(&req).unwrap_or_default())
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "loadtest.started",
            None,
            json!({ "run_id": id, "name": req.name, "bots": req.bots, "scenario": req.scenario, "url": url }),
        )
        .await;
    let live = active().lock().unwrap().clone().unwrap();
    let st = s.clone();
    tokio::spawn(async move {
        let report = run(&st, live.clone(), req.clone(), url).await;
        let state = if live.stopped.load(Ordering::Relaxed) {
            "stopped"
        } else if report["bots"]["connected"].as_u64().unwrap_or(0) == 0 {
            "failed"
        } else {
            "finished"
        };
        let _ = sqlx::query(
            "update musubi.loadtest_runs set state = $2, report = $3, finished_at = now() where id = $1",
        )
        .bind(id)
        .bind(state)
        .bind(&report)
        .execute(&st.db)
        .await;
        let _ = sqlx::query(
            "delete from musubi.loadtest_runs where id in (select id from musubi.loadtest_runs where state <> 'running' order by created_at desc offset $1)",
        )
        .bind(st.cfg.dev.loadtest.keep_runs as i64)
        .execute(&st.db)
        .await;
        st.events
            .emit(
                &format!("loadtest.{state}"),
                None,
                json!({ "run_id": id, "name": req.name, "summary": report["summary"] }),
            )
            .await;
        let mut a = active().lock().unwrap();
        if a.as_ref().is_some_and(|l| l.id == id) {
            *a = None;
        }
    });
    get(s, id).await
}

pub async fn stop(s: &AppState, id: Uuid) -> Result<Value> {
    let live = active().lock().unwrap().clone();
    match live {
        Some(l) if l.id == id => {
            l.stopped.store(true, Ordering::Relaxed);
            let _ = l.stop.send(true);
        }
        _ => {
            let st: Option<(String,)> =
                sqlx::query_as("select state from musubi.loadtest_runs where id = $1")
                    .bind(id)
                    .fetch_optional(&s.db)
                    .await
                    .map_err(Error::internal)?;
            match st {
                None => return Err(Error::NotFound(format!("loadtest run {id}"))),
                Some((state,)) => {
                    return Err(Error::Conflict(format!("run is {state}, nothing to stop")))
                }
            }
        }
    }
    // Give the runner a moment to persist the final report so the caller sees `stopped`.
    for _ in 0..50 {
        if active().lock().unwrap().as_ref().is_none_or(|l| l.id != id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    get(s, id).await
}

pub async fn delete(s: &AppState, id: Uuid) -> Result<()> {
    if active()
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|l| l.id == id)
    {
        return Err(Error::Conflict("stop the run before deleting it".into()));
    }
    let n = sqlx::query("delete from musubi.loadtest_runs where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("loadtest run {id}")));
    }
    Ok(())
}

type Row = (
    Uuid,
    String,
    String,
    String,
    Value,
    Value,
    time::OffsetDateTime,
    Option<time::OffsetDateTime>,
    Option<time::OffsetDateTime>,
);

fn row_json(r: Row) -> Value {
    json!({
        "id": r.0, "name": r.1, "state": r.2, "node": r.3, "request": r.4, "report": r.5,
        "created_at": r.6.unix_timestamp(),
        "started_at": r.7.map(|t| t.unix_timestamp()),
        "finished_at": r.8.map(|t| t.unix_timestamp()),
    })
}

pub async fn get(s: &AppState, id: Uuid) -> Result<Value> {
    let r: Option<Row> = sqlx::query_as(
        "select id, name, state, node, request, report, created_at, started_at, finished_at from musubi.loadtest_runs where id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = row_json(r.ok_or_else(|| Error::NotFound(format!("loadtest run {id}")))?);
    let live = active().lock().unwrap().clone();
    if let Some(l) = live.filter(|l| l.id == id) {
        v["state"] = json!(if l.stopped.load(Ordering::Relaxed) {
            "stopping"
        } else {
            "running"
        });
        v["live"] = l.snapshot.lock().unwrap().clone();
    }
    Ok(v)
}

pub async fn list(s: &AppState, state: Option<&str>, limit: i64) -> Result<Value> {
    let rows: Vec<Row> = sqlx::query_as(
        "select id, name, state, node, request, report, created_at, started_at, finished_at from musubi.loadtest_runs where ($1::text is null or state = $1) order by created_at desc limit $2",
    )
    .bind(state)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let active_id = active().lock().unwrap().as_ref().map(|l| l.id);
    let runs: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut v = row_json(r);
            v.as_object_mut().unwrap().remove("request");
            v["report"] = v["report"]["summary"].clone();
            v
        })
        .collect();
    Ok(json!({ "runs": runs, "active": active_id }))
}

/// Relative change of `b` vs `a` in percent (positive = `b` is larger).
fn delta_pct(a: f64, b: f64) -> Option<f64> {
    if a <= 0.0 {
        return None;
    }
    Some(((b - a) / a * 100.0 * 10.0).round() / 10.0)
}

pub async fn compare(s: &AppState, id: Uuid, base: Uuid, threshold_pct: f64) -> Result<Value> {
    let cur = get(s, id).await?;
    let bas = get(s, base).await?;
    for (label, v) in [("run", &cur), ("base", &bas)] {
        if v["report"]["summary"].is_null() {
            return Err(Error::Conflict(format!("{label} has no report yet")));
        }
    }
    let (c, b) = (&cur["report"]["summary"], &bas["report"]["summary"]);
    let mut regressions = Vec::new();
    let mut metrics = serde_json::Map::new();
    for (name, path, worse_if_up) in [
        ("rtt_p50_ms", ["rtt", "p50"], true),
        ("rtt_p99_ms", ["rtt", "p99"], true),
        ("connect_p99_ms", ["connect", "p99"], true),
        ("error_rate_pct", ["errors", "rate_pct"], true),
        ("throughput_rps", ["throughput", "requests_per_sec"], false),
    ] {
        let cv = c[path[0]][path[1]].as_f64().unwrap_or(0.0);
        let bv = b[path[0]][path[1]].as_f64().unwrap_or(0.0);
        let d = delta_pct(bv, cv);
        let regressed = match d {
            Some(d) if worse_if_up => d > threshold_pct,
            Some(d) => d < -threshold_pct,
            None => worse_if_up && cv > 0.0 && bv == 0.0 && name == "error_rate_pct",
        };
        if regressed {
            regressions.push(name);
        }
        metrics.insert(
            name.into(),
            json!({ "run": cv, "base": bv, "delta_pct": d, "regressed": regressed }),
        );
    }
    Ok(json!({
        "run": id, "base": base, "threshold_pct": threshold_pct,
        "verdict": if regressions.is_empty() { "ok" } else { "regressed" },
        "regressions": regressions, "metrics": metrics,
        "comparable": cur["request"]["scenario"] == bas["request"]["scenario"] && cur["request"]["bots"] == bas["request"]["bots"],
    }))
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn hist(v: &mut [f64]) -> Value {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r = |x: f64| (x * 100.0).round() / 100.0;
    json!({
        "count": v.len(),
        "p50": r(percentile(v, 50.0)), "p95": r(percentile(v, 95.0)), "p99": r(percentile(v, 99.0)),
        "max": r(v.last().copied().unwrap_or(0.0)),
        "mean": r(if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }),
    })
}

/// Bounded sample store: past `cap` samples every second one is kept (uniform thinning), so a
/// long run still fits in memory while percentiles stay representative.
struct Samples {
    v: Vec<f64>,
    cap: usize,
    stride: u64,
    seen: u64,
}

impl Samples {
    fn new(cap: usize) -> Self {
        Self {
            v: Vec::new(),
            cap,
            stride: 1,
            seen: 0,
        }
    }
    fn push(&mut self, x: f64) {
        self.seen += 1;
        if self.seen % self.stride != 0 {
            return;
        }
        if self.v.len() >= self.cap {
            self.v = self.v.iter().step_by(2).copied().collect();
            self.stride *= 2;
        }
        self.v.push(x);
    }
}

async fn run(s: &AppState, live: Arc<Live>, req: Request, url: String) -> Value {
    let (tx, mut rx) = mpsc::unbounded_channel::<Sample>();
    let t0 = Instant::now();
    let ramp = Duration::from_secs(req.ramp_secs);
    let deadline = t0 + ramp + Duration::from_secs(req.duration_secs);
    let req = Arc::new(req);
    // `match` groups: bots `k*size..(k+1)*size` share one relayed match, the first creates it.
    let groups: Vec<watch::Sender<Option<String>>> = (0..req.bots.div_ceil(req.match_size.max(1)))
        .map(|_| watch::channel(None).0)
        .collect();
    let mut stop_rx = live.stop.subscribe();
    let mut handles = Vec::with_capacity(req.bots as usize);
    for n in 0..req.bots {
        let delay = if req.ramp_secs == 0 || req.bots <= 1 {
            Duration::ZERO
        } else {
            ramp.mul_f64(n as f64 / req.bots as f64)
        };
        let token = match s
            .auth
            .authenticate_device(
                &crate::dev::seeded_device_id(
                    &s.cfg.http.server_key,
                    "loadbot",
                    &format!("{n:06}"),
                ),
                musubi_auth::AuthOptions {
                    create: true,
                    vars: [("loadtest".to_string(), json!(live.id))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(sess) => sess.access_token,
            Err(e) => {
                tracing::debug!(bot = n, error = %e, "loadtest: auth failed");
                let _ = tx.send(Sample {
                    kind: "auth_error",
                    ms: 0.0,
                    ok: false,
                });
                continue;
            }
        };
        let group = groups[(n / req.match_size.max(1)) as usize].clone();
        let leader = n % req.match_size.max(1) == 0;
        let b = Bot {
            n,
            url: url.clone(),
            token,
            req: req.clone(),
            tx: tx.clone(),
            live: live.clone(),
            stop: live.stop.subscribe(),
            deadline,
            group,
            leader,
        };
        handles.push(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            b.run().await;
        }));
    }
    drop(tx);
    // Aggregate: per-second timeline + overall histograms per kind.
    let mut rtt = Samples::new(200_000);
    let mut connect = Samples::new(20_000);
    let mut by_kind: HashMap<&'static str, Samples> = HashMap::new();
    let mut errors: HashMap<&'static str, u64> = HashMap::new();
    let mut ok_total = 0u64;
    let mut err_total = 0u64;
    let mut timeline = Vec::new();
    let mut window: Vec<f64> = Vec::new();
    let mut window_ok = 0u64;
    let mut window_err = 0u64;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.tick().await;
    let mut last_sent = 0u64;
    let mut last_recv = 0u64;
    let mut open = true;
    while open || !handles.is_empty() {
        tokio::select! {
            sm = rx.recv(), if open => match sm {
                Some(sm) => {
                    if sm.ok {
                        ok_total += 1;
                        window_ok += 1;
                        window.push(sm.ms);
                        if sm.kind == "connect" { connect.push(sm.ms); } else { rtt.push(sm.ms); }
                        by_kind.entry(sm.kind).or_insert_with(|| Samples::new(50_000)).push(sm.ms);
                    } else {
                        err_total += 1;
                        window_err += 1;
                        *errors.entry(sm.kind).or_default() += 1;
                    }
                }
                None => open = false,
            },
            _ = tick.tick() => {
                let sent = live.sent.load(Ordering::Relaxed);
                let recv = live.received.load(Ordering::Relaxed);
                window.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let entry = json!({
                    "t": t0.elapsed().as_secs(),
                    "connected": live.connected.load(Ordering::Relaxed),
                    "active": live.active.load(Ordering::Relaxed),
                    "sent": sent - last_sent, "received": recv - last_recv,
                    "ok": window_ok, "errors": window_err,
                    "p50": (percentile(&window, 50.0) * 100.0).round() / 100.0,
                    "p99": (percentile(&window, 99.0) * 100.0).round() / 100.0,
                });
                last_sent = sent;
                last_recv = recv;
                if timeline.len() < 4000 { timeline.push(entry.clone()); }
                window.clear();
                window_ok = 0;
                window_err = 0;
                *live.snapshot.lock().unwrap() = json!({
                    "elapsed_secs": t0.elapsed().as_secs(),
                    "phase": if Instant::now() < t0 + ramp { "ramping" } else if Instant::now() < deadline { "running" } else { "draining" },
                    "last_second": entry,
                    "totals": { "ok": ok_total, "errors": err_total, "sent": sent, "received": recv },
                });
                if !open { break; }
            },
            _ = stop_rx.changed() => {}
        }
        if !open {
            // Channel closed: every bot finished. Drain joins.
            for h in handles.drain(..) {
                let _ = h.await;
            }
        }
    }
    let elapsed = t0.elapsed().as_secs_f64().max(0.001);
    let measured = elapsed - req.ramp_secs as f64;
    let connected = live.connected.load(Ordering::Relaxed);
    let total = ok_total + err_total;
    let kinds: serde_json::Map<String, Value> = by_kind
        .iter_mut()
        .map(|(k, v)| ((*k).to_string(), hist(&mut v.v)))
        .collect();
    let summary = json!({
        "bots": { "requested": req.bots, "connected": connected, "failed": req.bots as u64 - connected },
        "duration_secs": (elapsed * 10.0).round() / 10.0,
        "rtt": hist(&mut rtt.v),
        "connect": hist(&mut connect.v),
        "throughput": {
            "requests_per_sec": (ok_total as f64 / measured.max(0.001) * 10.0).round() / 10.0,
            "sent": live.sent.load(Ordering::Relaxed), "received": live.received.load(Ordering::Relaxed),
        },
        "errors": {
            "total": err_total,
            "rate_pct": if total == 0 { 0.0 } else { (err_total as f64 / total as f64 * 1000.0).round() / 10.0 },
            "by_kind": errors,
        },
    });
    json!({
        "summary": summary,
        "bots": { "requested": req.bots, "connected": connected },
        "by_kind": kinds,
        "timeline": timeline,
        "url": url,
        "sampled": { "rtt_seen": rtt.seen, "rtt_kept": rtt.v.len() },
    })
}

struct Bot {
    n: u32,
    url: String,
    token: String,
    req: Arc<Request>,
    tx: mpsc::UnboundedSender<Sample>,
    live: Arc<Live>,
    stop: watch::Receiver<bool>,
    deadline: Instant,
    group: watch::Sender<Option<String>>,
    leader: bool,
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl Bot {
    fn sample(&self, kind: &'static str, ms: f64, ok: bool) {
        let _ = self.tx.send(Sample { kind, ms, ok });
    }

    fn encode(&self, cid: Option<String>, msg: ClientMessage) -> Option<Message> {
        let env = Envelope { cid, msg };
        if self.req.format == "json" {
            serde_json::to_string(&env).ok().map(Message::Text)
        } else {
            Encoding::Postcard
                .encode(&env)
                .ok()
                .map(|b| Message::Binary(b.to_vec()))
        }
    }

    fn decode(&self, m: Message) -> Option<Envelope<ServerMessage>> {
        match m {
            Message::Text(t) => serde_json::from_str(&t).ok(),
            Message::Binary(b) => Encoding::Postcard.decode(&b).ok(),
            _ => None,
        }
    }

    fn next_request(
        &self,
        tick: u64,
        match_id: Option<&str>,
        chan: Option<&str>,
    ) -> Option<(&'static str, ClientMessage)> {
        let r = &self.req;
        match r.scenario.as_str() {
            "ping" => Some(("ping", ClientMessage::Ping)),
            "rpc" => Some((
                "rpc",
                ClientMessage::Rpc {
                    id: r.rpc_id.clone(),
                    payload: r.rpc_payload.clone(),
                },
            )),
            "chat" => chan.map(|c| {
                (
                    "chat",
                    ClientMessage::ChannelMessageSend {
                        channel_id: c.to_string(),
                        content: json!({ "bot": self.n, "seq": tick }),
                        reply_to: None,
                    },
                )
            }),
            "match" => match_id.map(|m| {
                let mut data = vec![0u8; r.payload_bytes as usize];
                data[..8].copy_from_slice(&tick.to_le_bytes());
                (
                    "match",
                    ClientMessage::MatchDataSend {
                        match_id: m.to_string(),
                        op_code: 1,
                        data,
                        presences: vec![],
                        reliable: true,
                        channel: None,
                    },
                )
            }),
            _ => {
                let st = &r.steps[(tick as usize) % r.steps.len()];
                match st {
                    Step::Ping => Some(("ping", ClientMessage::Ping)),
                    Step::Rpc { id, payload } => Some((
                        "rpc",
                        ClientMessage::Rpc {
                            id: id.clone(),
                            payload: payload.clone(),
                        },
                    )),
                    Step::ChatSend { content } => chan.map(|c| {
                        (
                            "chat",
                            ClientMessage::ChannelMessageSend {
                                channel_id: c.to_string(),
                                content: if content.is_null() {
                                    json!({ "bot": self.n, "seq": tick })
                                } else {
                                    content.clone()
                                },
                                reply_to: None,
                            },
                        )
                    }),
                    Step::Status { status } => Some((
                        "status",
                        ClientMessage::StatusUpdate {
                            status: status.clone().or_else(|| Some(format!("bot {}", self.n))),
                            rich: None,
                            appear_offline: None,
                        },
                    )),
                    Step::Sleep { .. } => None,
                }
            }
        }
    }

    async fn run(mut self) {
        let t = Instant::now();
        let url = format!(
            "{}{}token={}&format={}",
            self.url,
            if self.url.contains('?') { "&" } else { "?" },
            self.token,
            self.req.format
        );
        let mut ws: Ws = match tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(url),
        )
        .await
        {
            Ok(Ok((ws, _))) => ws,
            _ => {
                self.sample("connect_error", 0.0, false);
                return;
            }
        };
        self.sample("connect", t.elapsed().as_secs_f64() * 1000.0, true);
        self.live.connected.fetch_add(1, Ordering::Relaxed);
        self.live.active.fetch_add(1, Ordering::Relaxed);
        let outcome = self.play(&mut ws).await;
        self.live.active.fetch_sub(1, Ordering::Relaxed);
        if let Some(kind) = outcome {
            self.sample(kind, 0.0, false);
        }
        let _ = ws.close(None).await;
    }

    /// Returns the terminal error kind, if the socket died before the deadline.
    async fn play(&mut self, ws: &mut Ws) -> Option<&'static str> {
        let r = self.req.clone();
        let needs_chat = r.scenario == "chat"
            || (r.scenario == "custom"
                && r.steps.iter().any(|s| matches!(s, Step::ChatSend { .. })));
        let mut chan: Option<String> = None;
        let mut match_id: Option<String> = None;
        // Setup phase (not measured): join the room / create or join the match.
        if needs_chat {
            let m = self.encode(
                Some("setup".into()),
                ClientMessage::ChannelJoin {
                    target: r.channel.clone(),
                    kind: ChannelKind::Room,
                    persistence: false,
                    hidden: false,
                },
            )?;
            if ws.send(m).await.is_err() {
                return Some("send_error");
            }
            chan = loop {
                match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
                    Ok(Some(Ok(m))) => match self.decode(m) {
                        Some(Envelope {
                            msg: ServerMessage::Channel { id, .. },
                            ..
                        }) => break Some(id),
                        Some(Envelope {
                            msg: ServerMessage::Error { .. },
                            cid,
                        }) if cid.as_deref() == Some("setup") => return Some("server_error"),
                        _ => continue,
                    },
                    _ => return Some("closed"),
                }
            };
        }
        if r.scenario == "match" {
            let join = if self.leader {
                ClientMessage::MatchCreate {
                    name: None,
                    suppress_presence: false,
                }
            } else {
                let mut rx = self.group.subscribe();
                let id = match tokio::time::timeout(
                    Duration::from_secs(15),
                    rx.wait_for(|v| v.is_some()),
                )
                .await
                {
                    Ok(Ok(v)) => v.clone().unwrap(),
                    _ => return Some("match_setup"),
                };
                ClientMessage::MatchJoin {
                    match_id: Some(id),
                    token: None,
                    metadata: Default::default(),
                    spectator: false,
                    delay_ticks: 0,
                }
            };
            let m = self.encode(Some("setup".into()), join)?;
            if ws.send(m).await.is_err() {
                return Some("send_error");
            }
            match_id = loop {
                match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
                    Ok(Some(Ok(m))) => match self.decode(m) {
                        Some(Envelope {
                            msg: ServerMessage::Match { match_id, .. },
                            ..
                        }) => break Some(match_id),
                        Some(Envelope {
                            msg: ServerMessage::Error { .. },
                            cid,
                        }) if cid.as_deref() == Some("setup") => return Some("server_error"),
                        _ => continue,
                    },
                    _ => return Some("closed"),
                }
            };
            if self.leader {
                self.group.send_replace(match_id.clone());
            }
        }
        let period = Duration::from_secs_f64(1.0 / r.rate_hz);
        let mut every = tokio::time::interval(period);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut in_flight: HashMap<String, (Instant, &'static str)> = HashMap::new();
        let mut tick = 0u64;
        let mut sleep_until: Option<Instant> = None;
        let reply_timeout = Duration::from_millis(r.reply_timeout_ms);
        let epoch = Instant::now();
        loop {
            if Instant::now() >= self.deadline {
                break;
            }
            tokio::select! {
                _ = every.tick() => {
                    // Expire stale requests.
                    let now = Instant::now();
                    in_flight.retain(|_, (t, _)| {
                        if now.duration_since(*t) > reply_timeout {
                            let _ = self.tx.send(Sample { kind: "timeout", ms: 0.0, ok: false });
                            false
                        } else {
                            true
                        }
                    });
                    if sleep_until.is_some_and(|u| now < u) {
                        continue;
                    }
                    sleep_until = None;
                    if let Step::Sleep { ms } = r.steps.get(if r.scenario == "custom" { (tick as usize) % r.steps.len() } else { usize::MAX }).unwrap_or(&Step::Ping) {
                        sleep_until = Some(now + Duration::from_millis(*ms));
                        tick += 1;
                        continue;
                    }
                    if in_flight.len() >= r.max_in_flight as usize {
                        self.sample("backpressure", 0.0, false);
                        continue;
                    }
                    let Some((kind, msg)) = self.next_request(tick, match_id.as_deref(), chan.as_deref()) else {
                        tick += 1;
                        continue;
                    };
                    tick += 1;
                    let cid = format!("{}", tick);
                    let Some(frame) = self.encode(Some(cid.clone()), msg) else { continue };
                    if ws.send(frame).await.is_err() {
                        return Some("send_error");
                    }
                    self.live.sent.fetch_add(1, Ordering::Relaxed);
                    if kind == "match" {
                        // Relayed data has no reply envelope: the echo carries our tick in the payload.
                        in_flight.insert(format!("m{}", tick - 1), (Instant::now(), kind));
                    } else {
                        in_flight.insert(cid, (Instant::now(), kind));
                    }
                }
                msg = ws.next() => {
                    let Some(Ok(m)) = msg else { return Some("closed") };
                    self.live.received.fetch_add(1, Ordering::Relaxed);
                    let Some(env) = self.decode(m) else { continue };
                    match &env.msg {
                        ServerMessage::MatchData { op_code: 1, data, .. } => {
                            if let Ok(b) = <[u8; 8]>::try_from(&data[..8.min(data.len())]) {
                                let key = format!("m{}", u64::from_le_bytes(b));
                                if let Some((t, kind)) = in_flight.remove(&key) {
                                    self.sample(kind, t.elapsed().as_secs_f64() * 1000.0, true);
                                }
                            }
                        }
                        ServerMessage::Error { .. } => {
                            if let Some(cid) = &env.cid {
                                in_flight.remove(cid);
                            }
                            self.sample("server_error", 0.0, false);
                        }
                        _ => {
                            if let Some((t, kind)) = env.cid.as_ref().and_then(|c| in_flight.remove(c)) {
                                self.sample(kind, t.elapsed().as_secs_f64() * 1000.0, true);
                            }
                        }
                    }
                }
                _ = self.stop.changed() => {
                    if *self.stop.borrow() { break; }
                }
                _ = tokio::time::sleep_until(self.deadline.into()) => break,
            }
        }
        let _ = epoch;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> musubi_core::config::LoadtestConfig {
        Default::default()
    }

    #[test]
    fn request_validation() {
        let ok = Request {
            scenario: "ping".into(),
            ..Default::default()
        };
        ok.validate(&cfg()).unwrap();
        let bad = [
            Request {
                bots: 0,
                ..Default::default()
            },
            Request {
                bots: 5000,
                ..Default::default()
            },
            Request {
                duration_secs: 0,
                ..Default::default()
            },
            Request {
                rate_hz: 0.0,
                ..Default::default()
            },
            Request {
                format: "xml".into(),
                ..Default::default()
            },
            Request {
                scenario: "world".into(),
                ..Default::default()
            },
            Request {
                scenario: "rpc".into(),
                ..Default::default()
            },
            Request {
                scenario: "match".into(),
                match_size: 0,
                ..Default::default()
            },
            Request {
                scenario: "custom".into(),
                ..Default::default()
            },
            Request {
                scenario: "custom".into(),
                steps: vec![Step::Sleep { ms: 0 }],
                ..Default::default()
            },
            Request {
                url: Some("http://x".into()),
                ..Default::default()
            },
        ];
        for b in bad {
            assert!(b.validate(&cfg()).is_err(), "{b:?}");
        }
        let remote = Request {
            url: Some("ws://prod.example:7350/ws".into()),
            ..Default::default()
        };
        assert!(matches!(
            remote.validate(&cfg()),
            Err(Error::PermissionDenied(_))
        ));
        remote
            .validate(&musubi_core::config::LoadtestConfig {
                allow_remote: true,
                ..cfg()
            })
            .unwrap();
        Request {
            url: Some("ws://127.0.0.1:1/ws".into()),
            ..Default::default()
        }
        .validate(&cfg())
        .unwrap();
        let steps: Vec<Step> = serde_json::from_value(json!([
            {"op": "ping"}, {"op": "rpc", "id": "f"}, {"op": "chat_send"}, {"op": "status"}, {"op": "sleep", "ms": 10}
        ]))
        .unwrap();
        assert_eq!(steps.len(), 5);
    }

    #[test]
    fn url_host_check_uses_a_real_parser() {
        for ok in [
            "ws://[::1]/ws",
            "ws://[::1]:7350/ws",
            "wss://localhost/ws?x=1",
        ] {
            Request {
                url: Some(ok.into()),
                ..Default::default()
            }
            .validate(&cfg())
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for remote in [
            "ws://localhost@evil.example/ws",
            "ws://127.0.0.1.evil.example/ws",
            "ws://[::2]/ws",
        ] {
            assert!(
                matches!(
                    Request {
                        url: Some(remote.into()),
                        ..Default::default()
                    }
                    .validate(&cfg()),
                    Err(Error::PermissionDenied(_))
                ),
                "{remote}"
            );
        }
        assert!(matches!(
            Request {
                url: Some("ws://".into()),
                ..Default::default()
            }
            .validate(&cfg()),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn percentiles_and_thinning() {
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(percentile(&v, 50.0), 51.0);
        assert_eq!(percentile(&v, 99.0), 99.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
        let mut s = Samples::new(100);
        for i in 0..1000 {
            s.push(i as f64);
        }
        assert!(s.v.len() <= 100 && s.v.len() >= 50, "{}", s.v.len());
        assert_eq!(s.seen, 1000);
        assert_eq!(delta_pct(100.0, 125.0), Some(25.0));
        assert_eq!(delta_pct(0.0, 5.0), None);
    }
}
