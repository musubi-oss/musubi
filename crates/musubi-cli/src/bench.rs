//! `musubi bench`: a small load generator so latency claims are measured, not asserted.
//! Every client authenticates with a device id, then loops its scenario until the deadline;
//! latencies are reported as p50/p95/p99/max with throughput and error counts.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt, TryStreamExt};
use musubi_proto::{ClientMessage, Envelope, ServerMessage};
use serde_json::{json, Value};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Opts {
    pub url: String,
    pub clients: usize,
    pub duration: Duration,
    pub scenario: String,
    /// `world` scenario: worlds to create, entities per world, client interest radius.
    pub worlds: usize,
    pub entities: usize,
    /// `world` scenario: simulators per world over the WS stream (1 = one HTTP-`apply` simulator);
    /// cells are partitioned between them and entities drift across the borders (handovers).
    pub sims: usize,
    pub radius: f64,
    /// Cap on deltas per second per world player (`None` = every tick).
    pub max_hz: Option<u32>,
    /// Wire encoding for the world players: `json` or `binary` (postcard).
    pub wire: String,
    /// `ws` or `quic` (+ `quic_addr`); QUIC skips certificate verification (bench only).
    pub transport: String,
    pub quic_addr: Option<String>,
    /// `WorldSubscribe.unreliable_updates`.
    pub unreliable: bool,
    pub json: bool,
    /// `match` scenario: players per relayed match (one creates, the rest join).
    pub match_size: usize,
}

/// Dev server key (`[auth] server_key` default) for the world scenario's simulator side.
const KEY: &str = "defaultkey";

/// Totals of the `world` scenario across all clients.
#[derive(Default)]
struct WorldTotals {
    frames: AtomicU64,
    bytes: AtomicU64,
    entities: AtomicU64,
    moves_ok: AtomicU64,
    moves_err: AtomicU64,
    moves_timeout: AtomicU64,
    moves_4xx: AtomicU64,
    moves_5xx: AtomicU64,
    apply_n: AtomicU64,
    apply_ms_sum: AtomicU64,
    apply_ms_max: AtomicU64,
    /// Simulator ticks that went out inside the window, and ticks dropped because eight
    /// batches were still in flight (the server answering slower than the simulator moves).
    sim_ticks: AtomicU64,
    sim_skipped: AtomicU64,
    /// Why a player gave up: WS handshake, first subscribe, stream closed/error, resubscribe.
    err_connect: AtomicU64,
    err_subscribe: AtomicU64,
    err_stream: AtomicU64,
    stream_frames: AtomicU64,
    stream_reconnects: AtomicU64,
    handover_acks: AtomicU64,
}

struct Arena {
    /// one per simulator; `sim` is `sims[0]`
    sims: Vec<uuid::Uuid>,
    name: String,
    sim: uuid::Uuid,
    side: f64,
    entities: Vec<String>,
}

/// Create a world, lease the cells covering a square arena and spawn `entities` in it.
async fn world_setup(
    http: &reqwest::Client,
    url: &str,
    name: &str,
    entities: usize,
    sims: usize,
) -> anyhow::Result<Arena> {
    let cell = 256.0;
    let side = ((entities.max(1) as f64).sqrt() * 20.0).max(cell);
    let r = http
        .post(format!("{url}/v1/server/worlds"))
        .header("x-musubi-server-key", KEY)
        .json(&json!({"name": name, "config": {"cell_size": cell}}))
        .send()
        .await?;
    anyhow::ensure!(r.status().is_success(), "create world: {}", r.status());
    let sims: Vec<uuid::Uuid> = (0..sims.max(1)).map(|_| uuid::Uuid::now_v7()).collect();
    let n = (side / cell).ceil() as i64;
    let owner = |x: i64, y: i64| ((x + y) as usize) % sims.len();
    let parts: Vec<Vec<Value>> = (0..sims.len())
        .map(|k| {
            (0..n)
                .flat_map(|x| (0..n).map(move |y| (x, y)))
                .filter(|&(x, y)| owner(x, y) == k)
                .map(|(x, y)| json!({"x": x, "y": y}))
                .collect()
        })
        .collect();
    let lease_all = |http: reqwest::Client| {
        let parts = parts.clone();
        let sims = sims.clone();
        let url = url.to_string();
        let name = name.to_string();
        async move {
            for (k, cells) in parts.iter().enumerate() {
                if cells.is_empty() {
                    continue;
                }
                let r = http
                    .post(format!("{url}/v1/server/worlds/{name}/lease"))
                    .header("x-musubi-server-key", KEY)
                    .json(&json!({"sim": sims[k], "cells": cells}))
                    .send()
                    .await?;
                anyhow::ensure!(r.status().is_success(), "lease: {}", r.status());
            }
            anyhow::Ok(())
        }
    };
    lease_all(http.clone()).await?;
    let mut ids = Vec::with_capacity(entities);
    for i in 0..entities {
        // thousands of serial spawns outlive `lease_ttl_ticks`: keep the lease alive meanwhile
        if i > 0 && i % 200 == 0 {
            lease_all(http.clone()).await?;
        }
        let (x, y) = arena_point(side, i as u64);
        let sim = sims[owner((x / cell) as i64, (y / cell) as i64)];
        let r = http
            .post(format!("{url}/v1/server/worlds/{name}/spawn"))
            .header("x-musubi-server-key", KEY)
            .json(&json!({"sim": sim, "x": x, "y": y, "components": {"hp": 100}, "persistent": false}))
            .send()
            .await?;
        anyhow::ensure!(r.status().is_success(), "spawn: {}", r.status());
        let v: Value = r.json().await?;
        ids.push(v["id"].as_str().unwrap_or_default().to_string());
    }
    Ok(Arena {
        name: name.to_string(),
        sim: sims[0],
        sims,
        side,
        entities: ids,
    })
}

/// Deterministic pseudo-random point in the arena (no rand dependency).
fn arena_point(side: f64, seed: u64) -> (f64, f64) {
    let h = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let x = (h >> 11) as f64 / (1u64 << 53) as f64;
    let y = (h.rotate_left(29) >> 11) as f64 / (1u64 << 53) as f64;
    (x * side, y * side)
}

/// Simulator side of the arena: renew the lease every 500 ms and, every tick (33 ms), move a
/// thirtieth of the entities so each one moves about once a second and every tick carries
/// changes (a plain HTTP `apply` per entity — the simplest, not the fastest, simulator). Paced
/// by the clock, not by the replies: a slow `apply` shows up as latency, not as a slower
/// simulator (which used to read as "fewer frames per player").
async fn world_sim(
    http: reqwest::Client,
    url: String,
    arena: Arc<Arena>,
    mut deadline: tokio::sync::watch::Receiver<Option<Instant>>,
    totals: Arc<WorldTotals>,
) {
    let n = arena.entities.len();
    let cell = 256.0;
    let k = (arena.side / cell).ceil() as i64;
    let cells: Vec<Value> = (0..k)
        .flat_map(|x| (0..k).map(move |y| json!({"x": x, "y": y})))
        .collect();
    let mut step = 0u64;
    let mut last_lease = Instant::now();
    let mut iv = tokio::time::interval(Duration::from_millis(33));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        let window = *deadline.borrow_and_update();
        if window.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        iv.tick().await;
        if last_lease.elapsed() >= Duration::from_millis(500) {
            last_lease = Instant::now();
            let _ = http
                .post(format!("{url}/v1/server/worlds/{}/lease", arena.name))
                .header("x-musubi-server-key", KEY)
                .json(&json!({"sim": arena.sim, "cells": cells}))
                .timeout(Duration::from_secs(2))
                .send()
                .await;
        }
        if n == 0 {
            continue;
        }
        let batch = (n / 30).max(1);
        let mut futs = Vec::with_capacity(batch);
        for j in 0..batch {
            let i = ((step as usize) * batch + j) % n;
            let (x, y) = arena_point(arena.side, step.wrapping_mul(7919).wrapping_add(i as u64));
            let (http, url, arena, totals) =
                (http.clone(), url.clone(), arena.clone(), totals.clone());
            futs.push(async move {
                let t0 = Instant::now();
                let res = http
                    .post(format!("{url}/v1/server/worlds/{}/apply", arena.name))
                    .header("x-musubi-server-key", KEY)
                    .json(&json!({"sim": arena.sim, "entity": arena.entities[i], "x": x, "y": y}))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                let ms = t0.elapsed().as_millis() as u64;
                totals.apply_n.fetch_add(1, Ordering::Relaxed);
                totals.apply_ms_sum.fetch_add(ms, Ordering::Relaxed);
                totals.apply_ms_max.fetch_max(ms, Ordering::Relaxed);
                if window.is_none() {
                    // warming up while players authenticate: keep moving, do not count
                    return;
                }
                match res {
                    Ok(r) if r.status().is_success() => {
                        totals.moves_ok.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(r) => {
                        totals.moves_err.fetch_add(1, Ordering::Relaxed);
                        if r.status().is_client_error() {
                            totals.moves_4xx.fetch_add(1, Ordering::Relaxed);
                        } else {
                            totals.moves_5xx.fetch_add(1, Ordering::Relaxed);
                        }
                        if totals.moves_err.load(Ordering::Relaxed) <= 3 {
                            let st = r.status();
                            let body = r.text().await.unwrap_or_default();
                            eprintln!("sim apply failed: {st} {body}");
                        }
                    }
                    Err(e) => {
                        totals.moves_err.fetch_add(1, Ordering::Relaxed);
                        if e.is_timeout() {
                            totals.moves_timeout.fetch_add(1, Ordering::Relaxed);
                        } else if totals.moves_err.load(Ordering::Relaxed) <= 3 {
                            eprintln!("sim apply failed: {e}");
                        }
                    }
                }
            });
        }
        if inflight.load(Ordering::Relaxed) >= 8 {
            totals.sim_skipped.fetch_add(1, Ordering::Relaxed);
        } else {
            if window.is_some() {
                totals.sim_ticks.fetch_add(1, Ordering::Relaxed);
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let inflight = inflight.clone();
            tokio::spawn(async move {
                futures::future::join_all(futs).await;
                inflight.fetch_sub(1, Ordering::Relaxed);
            });
        }
        step += 1;
    }
}

/// First frames of a world run that are not deltas are printed (encoding/subscription problems).
static FRAME_DEBUG: AtomicU64 = AtomicU64::new(0);
static STREAM_DEBUG: AtomicU64 = AtomicU64::new(0);
static CONNECT_DEBUG: AtomicU64 = AtomicU64::new(0);

struct Player {
    url: String,
    wire: String,
    transport: String,
    quic_addr: Option<String>,
    unreliable: bool,
    token: String,
    arena: Arc<Arena>,
    radius: f64,
    max_hz: Option<u32>,
    seed: u64,
    deadline: Instant,
    tx: mpsc::UnboundedSender<Sample>,
    totals: Arc<WorldTotals>,
}

/// A player: subscribe at a point of the arena, drift it every 200 ms, count delta frames; each
/// sample is the gap between consecutive frames (33 ms at 30 Hz when the server keeps up).
async fn world_loop(p: Player) {
    let Player {
        url,
        wire,
        transport,
        quic_addr,
        unreliable,
        token,
        arena,
        radius,
        max_hz,
        seed,
        deadline,
        tx,
        totals,
    } = p;
    let enc = musubi_proto::Encoding::parse(&wire).unwrap_or_default();
    let mut ws = match Link::connect(&transport, &url, quic_addr.as_deref(), &token, &wire).await {
        Ok(c) => c,
        Err(e) => {
            if CONNECT_DEBUG.fetch_add(1, Ordering::Relaxed) < 3 {
                eprintln!("world: connect failed: {e}");
            }
            totals.err_connect.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(sample("world", Instant::now(), false, false));
            return;
        }
    };
    let subscribe = |step: u64| {
        // random walk, not teleport: each move is a short hop, so the view keeps most of what
        // the client already knows (a real player moves; it does not respawn across the map)
        let hop = arena.side * 0.05;
        let (mut x, mut y) = arena_point(arena.side, seed);
        for k in 1..=step {
            let (hx, hy) = arena_point(1.0, seed ^ k.wrapping_mul(0x9E37_79B9));
            x = (x + (hx - 0.5) * 2.0 * hop).rem_euclid(arena.side);
            y = (y + (hy - 0.5) * 2.0 * hop).rem_euclid(arena.side);
        }
        let env = Envelope {
            cid: None,
            msg: ClientMessage::WorldSubscribe {
                world: arena.name.clone(),
                x,
                y,
                radius,
                budget_bytes: None,
                filter: None,
                max_hz,
                unreliable_updates: unreliable,
            },
        };
        match enc {
            musubi_proto::Encoding::Json => {
                tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&env).unwrap())
            }
            _ => {
                tokio_tungstenite::tungstenite::Message::Binary(enc.encode(&env).unwrap().to_vec())
            }
        }
    };
    if ws.send(subscribe(0)).await.is_err() {
        totals.err_subscribe.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(sample("world", Instant::now(), false, false));
        return;
    }
    let mut last_frame: Option<Instant> = None;
    let mut next_move = Instant::now() + Duration::from_millis(200);
    let mut step = 1u64;
    while Instant::now() < deadline {
        let wait = next_move.saturating_duration_since(Instant::now());
        match tokio::time::timeout(wait.max(Duration::from_millis(1)), ws.next()).await {
            Ok(Some(Ok(m @ tokio_tungstenite::tungstenite::Message::Text(_))))
            | Ok(Some(Ok(m @ tokio_tungstenite::tungstenite::Message::Binary(_)))) => {
                let bytes = m.into_data();
                totals
                    .bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                let decoded = enc.decode::<Envelope<ServerMessage>>(&bytes);
                if FRAME_DEBUG.fetch_add(1, Ordering::Relaxed) < 3 {
                    match &decoded {
                        Err(e) => eprintln!("world: frame decode failed: {e}"),
                        Ok(env) if !matches!(env.msg, ServerMessage::WorldDelta { .. }) => {
                            eprintln!("world: unexpected frame: {:?}", env.msg)
                        }
                        _ => {}
                    }
                }
                if let Ok(Envelope {
                    msg:
                        ServerMessage::WorldDelta {
                            spawned, updated, ..
                        },
                    ..
                }) = decoded
                {
                    totals.frames.fetch_add(1, Ordering::Relaxed);
                    totals
                        .entities
                        .fetch_add((spawned.len() + updated.len()) as u64, Ordering::Relaxed);
                    let now = Instant::now();
                    if let Some(prev) = last_frame {
                        let _ = tx.send(Sample {
                            scenario: "world",
                            ms: now.duration_since(prev).as_secs_f64() * 1e3,
                            ok: true,
                            throttled: false,
                        });
                    }
                    last_frame = Some(now);
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(other) => {
                if STREAM_DEBUG.fetch_add(1, Ordering::Relaxed) < 3 {
                    match other {
                        Some(Err(e)) => eprintln!("world: stream error: {e}"),
                        _ => eprintln!("world: stream ended by the server"),
                    }
                }
                totals.err_stream.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(sample("world", Instant::now(), false, false));
                return;
            }
            Err(_) => {
                next_move = Instant::now() + Duration::from_millis(200);
                if ws.send(subscribe(step)).await.is_err() {
                    totals.err_stream.fetch_add(1, Ordering::Relaxed);
                    let _ = tx.send(sample("world", Instant::now(), false, false));
                    return;
                }
                step += 1;
            }
        }
    }
    let _ = ws.close(None).await;
}

/// One of `sims` simulators of an arena over the WS stream (`/v1/server/worlds/{w}/stream`):
/// leases its share of the grid, and on every `tick` frame moves a thirtieth of the entities it
/// owns a few units along x (wrapping at the arena edge), so each entity crosses a cell border —
/// and, with several simulators, changes owner — every few seconds. Answers `handover_request`
/// events with `handover_ack`, re-leases every half second, reconnects on drop.
async fn world_streamer(
    url: String,
    arena: Arc<Arena>,
    k: usize,
    mut deadline: tokio::sync::watch::Receiver<Option<Instant>>,
    totals: Arc<WorldTotals>,
) {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let cell = 256.0;
    let sims = arena.sims.len();
    let sim = arena.sims[k];
    let n = (arena.side / cell).ceil() as i64;
    let cells: Vec<Value> = (0..n)
        .flat_map(|x| (0..n).map(move |y| (x, y)))
        .filter(|&(x, y)| ((x + y) as usize) % sims == k)
        .map(|(x, y)| json!({"x": x, "y": y}))
        .collect();
    let ws_base = url.replacen("http", "ws", 1);
    let past = |d: &tokio::sync::watch::Receiver<Option<Instant>>| {
        d.borrow().is_some_and(|t| Instant::now() >= t)
    };
    let mut frame_no = 0u64;
    let mut first = true;
    // direction per entity: bounce at the arena edges instead of wrapping (a wrap is a teleport
    // into a cell this simulator does not own and is rightly refused)
    let mut dir: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    while !past(&deadline) {
        if !first {
            totals.stream_reconnects.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        first = false;
        let Ok(mut req) = format!("{ws_base}/v1/server/worlds/{}/stream?sim={sim}", arena.name)
            .into_client_request()
        else {
            return;
        };
        req.headers_mut()
            .insert("x-musubi-server-key", KEY.parse().unwrap());
        let Ok(Ok((mut ws, _))) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(req),
        )
        .await
        else {
            totals.err_stream.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let lease = json!({"op": "lease", "cells": cells}).to_string();
        if ws.send(Message::Text(lease.clone())).await.is_err() {
            continue;
        }
        let mut last_lease = Instant::now();
        loop {
            let m = tokio::select! {
                m = ws.next() => m,
                _ = deadline.changed() => { if past(&deadline) { return } continue }
                _ = tokio::time::sleep(Duration::from_secs(3)) => break,
            };
            let Some(Ok(m)) = m else { break };
            if past(&deadline) {
                return;
            }
            let Message::Text(t) = m else { continue };
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v["type"] == "tick" {
                frame_no += 1;
                totals.stream_frames.fetch_add(1, Ordering::Relaxed);
                totals.sim_ticks.fetch_add(1, Ordering::Relaxed);
                let mut out: Vec<String> = Vec::new();
                for ev in v["events"].as_array().into_iter().flatten() {
                    if ev["type"] == "handover_request" {
                        totals.handover_acks.fetch_add(1, Ordering::Relaxed);
                        out.push(json!({"op": "handover_ack", "entity": ev["entity"]}).to_string());
                    }
                }
                let mut i = 0u64;
                for c in v["cells"].as_array().into_iter().flatten() {
                    for e in c["owned"].as_array().into_iter().flatten() {
                        i += 1;
                        if (i + frame_no) % 30 != 0 {
                            continue;
                        }
                        let (Some(id), Some(x), Some(y)) =
                            (e["id"].as_str(), e["x"].as_f64(), e["y"].as_f64())
                        else {
                            continue;
                        };
                        let d = dir.entry(id.to_string()).or_insert(6.0);
                        if x + *d <= 0.0 || x + *d >= arena.side {
                            *d = -*d;
                        }
                        let nx = (x + *d).clamp(0.0, arena.side - 0.001);
                        out.push(
                            json!({"op": "apply", "entity": id, "x": nx, "y": y, "components": {"hp": (frame_no % 100) as i64}})
                                .to_string(),
                        );
                        totals.moves_ok.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if last_lease.elapsed() >= Duration::from_millis(500) {
                    last_lease = Instant::now();
                    out.push(lease.clone());
                }
                let mut failed = false;
                for o in out {
                    if ws.send(Message::Text(o)).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            } else if v["error"].is_string() {
                totals.moves_err.fetch_add(1, Ordering::Relaxed);
                if totals.moves_err.load(Ordering::Relaxed) <= 3 {
                    eprintln!("sim stream error: {t}");
                }
            }
        }
    }
}

/// Sum of a counter over the bench worlds, from `/metrics`.
async fn scrape_counter(http: &reqwest::Client, url: &str, prefix: &str, metric: &str) -> f64 {
    let Ok(r) = http.get(format!("{url}/metrics")).send().await else {
        return 0.0;
    };
    let Ok(body) = r.text().await else { return 0.0 };
    body.lines()
        .filter(|l| l.starts_with(metric) && l.contains(&format!("world=\"{prefix}")))
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

/// World tick duration from `/metrics`: mean, p50 and p99 (ms) over the bench worlds, from the
/// Prometheus histogram (buckets) or summary (quantiles), whichever the exporter renders.
async fn scrape_tick(http: &reqwest::Client, url: &str, prefix: &str, metric: &str) -> Value {
    let Ok(r) = http.get(format!("{url}/metrics")).send().await else {
        return Value::Null;
    };
    let Ok(text) = r.text().await else {
        return Value::Null;
    };
    let mut buckets: std::collections::BTreeMap<String, f64> = Default::default();
    let (mut sum, mut count) = (0.0, 0.0);
    let mut quantiles = serde_json::Map::new();
    for line in text.lines() {
        if !line.starts_with(metric) || !line.contains(&format!("world=\"{prefix}")) {
            continue;
        }
        let Some((lhs, val)) = line.rsplit_once(' ') else {
            continue;
        };
        let val: f64 = val.parse().unwrap_or(0.0);
        if lhs.starts_with(&format!("{metric}_sum")) {
            sum += val;
        } else if lhs.starts_with(&format!("{metric}_count")) {
            count += val;
        } else if let Some(le) = lhs.split("le=\"").nth(1) {
            let le = le.split('"').next().unwrap_or("+Inf").to_string();
            *buckets.entry(le).or_insert(0.0) += val;
        } else if let Some(q) = lhs.split("quantile=\"").nth(1) {
            let q = q.split('"').next().unwrap_or("").to_string();
            quantiles.insert(q, json!(val * 1e3));
        }
    }
    let pct = |p: f64| -> Option<f64> {
        let mut bs: Vec<(f64, f64)> = buckets
            .iter()
            .map(|(le, c)| (le.parse::<f64>().unwrap_or(f64::INFINITY), *c))
            .collect();
        bs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let total = bs.last()?.1;
        bs.iter()
            .find(|(_, c)| *c >= total * p)
            .map(|(le, _)| le * 1e3)
    };
    json!({
        "ticks": count,
        "mean_ms": if count > 0.0 { sum / count * 1e3 } else { 0.0 },
        "p50_ms": pct(0.5),
        "p99_ms": pct(0.99),
        "quantiles_ms": quantiles,
    })
}

#[derive(Debug)]
struct Sample {
    scenario: &'static str,
    ms: f64,
    ok: bool,
    /// Rejected by the per-account rate limit (`[limits]`): not a server fault, reported apart.
    throttled: bool,
}

fn sample(scenario: &'static str, t: Instant, ok: bool, throttled: bool) -> Sample {
    Sample {
        scenario,
        ms: t.elapsed().as_secs_f64() * 1e3,
        ok,
        throttled,
    }
}

/// Back off briefly when throttled so a client does not spin on 429s.
const THROTTLE_PAUSE: Duration = Duration::from_millis(20);

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Serialised `ping` envelope, shared with `musubi soak`.
pub(crate) fn ping_text() -> String {
    serde_json::to_string(&Envelope {
        cid: None,
        msg: ClientMessage::Ping,
    })
    .unwrap()
}

pub(crate) fn ws_url(http: &str) -> String {
    let base = http.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    };
    format!("{ws}/ws")
}

pub(crate) async fn auth(http: &reqwest::Client, url: &str, id: &str) -> anyhow::Result<String> {
    let v: Value = http
        .post(format!("{url}/v1/auth/device"))
        .json(&json!({ "id": id }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    v["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no access_token in {v}"))
}

async fn http_loop(
    http: reqwest::Client,
    url: String,
    token: String,
    deadline: Instant,
    tx: mpsc::UnboundedSender<Sample>,
) {
    while Instant::now() < deadline {
        let t = Instant::now();
        let status = http
            .get(format!("{url}/v1/account"))
            .bearer_auth(&token)
            .send()
            .await
            .map(|r| r.status().as_u16())
            .unwrap_or(0);
        let _ = tx.send(sample("http", t, status == 200, status == 429));
        if status == 429 {
            tokio::time::sleep(THROTTLE_PAUSE).await;
        }
    }
}

async fn storage_loop(
    http: reqwest::Client,
    url: String,
    token: String,
    deadline: Instant,
    tx: mpsc::UnboundedSender<Sample>,
) {
    let mut i = 0u64;
    while Instant::now() < deadline {
        i += 1;
        let (path, body) = if i % 2 == 1 {
            (
                "/v1/storage/write",
                json!({ "objects": [{ "collection": "bench", "key": "k", "value": { "n": i } }] }),
            )
        } else {
            (
                "/v1/storage/read",
                json!({ "objects": [{ "collection": "bench", "key": "k" }] }),
            )
        };
        let t = Instant::now();
        let status = http
            .post(format!("{url}{path}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map(|r| r.status().as_u16())
            .unwrap_or(0);
        let _ = tx.send(sample("storage", t, status == 200, status == 429));
        if status == 429 {
            tokio::time::sleep(THROTTLE_PAUSE).await;
        }
    }
}

async fn ws_loop(url: String, token: String, deadline: Instant, tx: mpsc::UnboundedSender<Sample>) {
    let Ok((mut ws, _)) =
        tokio_tungstenite::connect_async(format!("{}?token={token}&format=json", ws_url(&url)))
            .await
    else {
        let _ = tx.send(sample("ws", Instant::now(), false, false));
        return;
    };
    let throttled_code = musubi_core::Error::ResourceExhausted(String::new()).code();
    let ping = ping_text();
    while Instant::now() < deadline {
        let t = Instant::now();
        if ws
            .send(tokio_tungstenite::tungstenite::Message::Text(ping.clone()))
            .await
            .is_err()
        {
            let _ = tx.send(sample("ws", t, false, false));
            return;
        }
        // Ok(true) pong, Ok(false) throttled, Err(()) lost
        let outcome = loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt)))) => {
                    match serde_json::from_str::<Envelope<ServerMessage>>(&txt).map(|e| e.msg) {
                        Ok(ServerMessage::Pong) => break Ok(true),
                        Ok(ServerMessage::Error { code, .. }) if code == throttled_code => {
                            break Ok(false)
                        }
                        _ => continue,
                    }
                }
                Ok(Some(Ok(_))) => continue,
                _ => break Err(()),
            }
        };
        match outcome {
            Ok(true) => {
                let _ = tx.send(sample("ws", t, true, false));
            }
            Ok(false) => {
                let _ = tx.send(sample("ws", t, false, true));
                tokio::time::sleep(THROTTLE_PAUSE).await;
            }
            Err(()) => {
                let _ = tx.send(sample("ws", t, false, false));
                return;
            }
        }
    }
    let _ = ws.close(None).await;
}

type MatchChannel = (
    tokio::sync::watch::Sender<Option<String>>,
    tokio::sync::watch::Receiver<Option<String>>,
);

/// A relayed match shared by `match_size` bench players: the leader creates it and publishes
/// the id, the others join once it is there.
struct MatchGroup {
    leader: bool,
    tx: tokio::sync::watch::Sender<Option<String>>,
    rx: tokio::sync::watch::Receiver<Option<String>>,
}

/// Process-wide epoch so a send time can travel inside match data and be read by another task.
fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(url: &str, token: &str) -> Option<WsStream> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let req = format!("{}?token={token}&format=json", ws_url(url))
        .into_client_request()
        .ok()?;
    let host = req.uri().host()?.to_string();
    let port = req.uri().port_u16().unwrap_or(80);
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .ok()?;
    // like a real game client: no Nagle, or the server's fan-out stalls on our delayed ACKs
    let _ = tcp.set_nodelay(true);
    let stream = tokio_tungstenite::MaybeTlsStream::Plain(tcp);
    tokio_tungstenite::client_async(req, stream)
        .await
        .ok()
        .map(|(ws, _)| ws)
}

fn envelope(msg: ClientMessage) -> tokio_tungstenite::tungstenite::Message {
    tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&Envelope { cid: None, msg }).unwrap(),
    )
}

/// Next server message within `wait`; `None` on close, error or timeout.
async fn ws_recv(ws: &mut WsStream, wait: Duration) -> Option<ServerMessage> {
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt)))) => {
                if let Ok(e) = serde_json::from_str::<Envelope<ServerMessage>>(&txt) {
                    return Some(e.msg);
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

/// `chat`: 50 clients share a room; each measures send → own echo (the room fan-out to the other
/// 49 rides along as server load).
async fn chat_loop(
    url: String,
    token: String,
    room: String,
    deadline: Instant,
    tx: mpsc::UnboundedSender<Sample>,
) {
    let fail = |t: Instant| {
        let _ = tx.send(sample("chat", t, false, false));
    };
    let Some(mut ws) = ws_connect(&url, &token).await else {
        return fail(Instant::now());
    };
    if ws
        .send(envelope(ClientMessage::ChannelJoin {
            target: room,
            kind: musubi_proto::ChannelKind::Room,
            persistence: false,
            hidden: false,
        }))
        .await
        .is_err()
    {
        return fail(Instant::now());
    }
    let channel_id = loop {
        match ws_recv(&mut ws, Duration::from_secs(10)).await {
            Some(ServerMessage::Channel { id, .. }) => break id,
            Some(_) => continue,
            None => return fail(Instant::now()),
        }
    };
    let tag = uuid::Uuid::now_v7().simple().to_string();
    let throttled_code = musubi_core::Error::ResourceExhausted(String::new()).code();
    let mut seq: u64 = 0;
    while Instant::now() < deadline {
        seq += 1;
        let t = Instant::now();
        if ws
            .send(envelope(ClientMessage::ChannelMessageSend {
                channel_id: channel_id.clone(),
                content: json!({ "tag": tag, "seq": seq }),
                reply_to: None,
            }))
            .await
            .is_err()
        {
            return fail(t);
        }
        let outcome = loop {
            match ws_recv(&mut ws, Duration::from_secs(5)).await {
                Some(ServerMessage::ChannelMessage(m))
                    if m.content["tag"] == json!(tag) && m.content["seq"] == json!(seq) =>
                {
                    break Some(true)
                }
                Some(ServerMessage::Error { code, .. }) if code == throttled_code => {
                    break Some(false)
                }
                Some(_) => continue,
                None => break None,
            }
        };
        match outcome {
            Some(true) => {
                let _ = tx.send(sample("chat", t, true, false));
            }
            Some(false) => {
                let _ = tx.send(sample("chat", t, false, true));
                tokio::time::sleep(THROTTLE_PAUSE).await;
            }
            None => return fail(t),
        }
    }
    let _ = ws.close(None).await;
}

/// `match`: `match_size` players in a relayed match, each sending its input at 30 Hz; the
/// sample is the relay latency measured at the receivers (send time travels in the payload), so
/// a group of one produces no samples.
async fn match_loop(
    url: String,
    token: String,
    group: Option<MatchGroup>,
    deadline: Instant,
    tx: mpsc::UnboundedSender<Sample>,
) {
    let fail = |t: Instant| {
        let _ = tx.send(sample("match", t, false, false));
    };
    let Some(mut group) = group else {
        return fail(Instant::now());
    };
    let Some(mut ws) = ws_connect(&url, &token).await else {
        return fail(Instant::now());
    };
    let join = if group.leader {
        ClientMessage::MatchCreate {
            name: None,
            suppress_presence: false,
        }
    } else {
        let id =
            match tokio::time::timeout(Duration::from_secs(15), group.rx.wait_for(|v| v.is_some()))
                .await
            {
                Ok(Ok(v)) => v.clone().unwrap(),
                _ => return fail(Instant::now()),
            };
        ClientMessage::MatchJoin {
            match_id: Some(id),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        }
    };
    if ws.send(envelope(join)).await.is_err() {
        return fail(Instant::now());
    }
    let match_id = loop {
        match ws_recv(&mut ws, Duration::from_secs(10)).await {
            Some(ServerMessage::Match { match_id, .. }) => break match_id,
            Some(_) => continue,
            None => return fail(Instant::now()),
        }
    };
    if group.leader {
        let _ = group.tx.send(Some(match_id.clone()));
    }
    let epoch = epoch();
    let mut every = tokio::time::interval(Duration::from_millis(33));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while Instant::now() < deadline {
        tokio::select! {
            _ = every.tick() => {
                let nanos = epoch.elapsed().as_nanos() as u64;
                if ws
                    .send(envelope(ClientMessage::MatchDataSend {
                        match_id: match_id.clone(),
                        op_code: 1,
                        data: nanos.to_le_bytes().to_vec(),
                        presences: vec![],
                        reliable: true, channel: None,
                    }))
                    .await
                    .is_err()
                {
                    return fail(Instant::now());
                }
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt))) => {
                        if let Ok(Envelope { msg: ServerMessage::MatchData { op_code: 1, data, .. }, .. }) =
                            serde_json::from_str::<Envelope<ServerMessage>>(&txt)
                        {
                            if let Ok(b) = <[u8; 8]>::try_from(data.as_slice()) {
                                let sent = epoch + Duration::from_nanos(u64::from_le_bytes(b));
                                let _ = tx.send(sample("match", sent, true, false));
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => return fail(Instant::now()),
                }
            }
        }
    }
    let _ = ws.close(None).await;
}

pub async fn run(o: Opts) -> anyhow::Result<()> {
    let url = o.url.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(o.clients)
        .build()?;
    let scenarios: Vec<&'static str> = match o.scenario.as_str() {
        "all" => vec!["http", "storage", "ws", "chat", "match"],
        "http" => vec!["http"],
        "storage" => vec!["storage"],
        "ws" => vec!["ws"],
        "chat" => vec!["chat"],
        "match" => vec!["match"],
        "world" => vec!["world"],
        other => {
            anyhow::bail!("unknown scenario {other}; use http|storage|ws|chat|match|world|all")
        }
    };
    let match_size = o.match_size.max(1);
    let mut match_groups: Vec<MatchChannel> = Vec::new();
    let run_id = uuid::Uuid::now_v7().simple().to_string();
    let totals = Arc::new(WorldTotals::default());
    let mut arenas: Vec<Arc<Arena>> = Vec::new();
    // the simulators start as soon as their arena exists: leases (2 s) must stay alive while
    // the players authenticate; the measurement window opens later through `window_tx`
    let (window_tx, window_rx) = tokio::sync::watch::channel::<Option<Instant>>(None);
    let mut sims = Vec::new();
    if scenarios.contains(&"world") {
        let t = Instant::now();
        for i in 0..o.worlds.max(1) {
            let name = format!("bench-{run_id}-{i}");
            let arena = Arc::new(world_setup(&http, &url, &name, o.entities, o.sims).await?);
            if o.sims > 1 {
                for k in 0..o.sims {
                    sims.push(tokio::spawn(world_streamer(
                        url.clone(),
                        arena.clone(),
                        k,
                        window_rx.clone(),
                        totals.clone(),
                    )));
                }
            } else {
                sims.push(tokio::spawn(world_sim(
                    http.clone(),
                    url.clone(),
                    arena.clone(),
                    window_rx.clone(),
                    totals.clone(),
                )));
            }
            arenas.push(arena);
        }
        if !o.json {
            eprintln!(
                "world setup: {} worlds x {} entities in {:.1}s",
                arenas.len(),
                o.entities,
                t.elapsed().as_secs_f64()
            );
        }
    }
    let t0 = Instant::now();
    // Logins go 64 at a time: one at a time makes a 10k setup take minutes, all at once
    // measures the auth path's burst behaviour instead of the worlds.
    let tokens: Vec<String> = futures::stream::iter(0..o.clients)
        .map(|i| {
            let http = http.clone();
            let url = url.clone();
            let id = format!("bench-{run_id}-{i}");
            async move { auth(&http, &url, &id).await }
        })
        .buffered(64)
        .try_collect()
        .await?;
    let auth_ms = t0.elapsed().as_secs_f64() * 1e3;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let deadline = Instant::now() + o.duration;
    let _ = window_tx.send(Some(deadline));
    let mut tasks = sims;
    for (i, token) in tokens.into_iter().enumerate() {
        let sc = scenarios[i % scenarios.len()];
        let (http, url, tx) = (http.clone(), url.clone(), tx.clone());
        // ordinal among the clients of this scenario: chat rooms of 50, matches of `match_size`
        let j = i / scenarios.len();
        let room = format!("bench-{run_id}-{}", j / 50);
        let group = if sc == "match" {
            let g = j / match_size;
            while match_groups.len() <= g {
                match_groups.push(tokio::sync::watch::channel(None));
            }
            let (gtx, grx) = &match_groups[g];
            Some(MatchGroup {
                leader: j % match_size == 0,
                tx: gtx.clone(),
                rx: grx.clone(),
            })
        } else {
            None
        };
        let arena = arenas.get(i % arenas.len().max(1)).cloned();
        let (radius, totals) = (o.radius, totals.clone());
        let max_hz = o.max_hz;
        let transport = o.transport.clone();
        let quic_addr = o.quic_addr.clone();
        let unreliable = o.unreliable;
        let wire = o.wire.clone();
        tasks.push(tokio::spawn(async move {
            match sc {
                "http" => http_loop(http, url, token, deadline, tx).await,
                "storage" => storage_loop(http, url, token, deadline, tx).await,
                "chat" => chat_loop(url, token, room, deadline, tx).await,
                "match" => match_loop(url, token, group, deadline, tx).await,
                "world" => {
                    world_loop(Player {
                        url,
                        wire,
                        transport: transport.clone(),
                        quic_addr: quic_addr.clone(),
                        unreliable,
                        token,
                        arena: arena.unwrap(),
                        radius,
                        max_hz,
                        seed: i as u64,
                        deadline,
                        tx,
                        totals,
                    })
                    .await
                }
                _ => ws_loop(url, token, deadline, tx).await,
            }
        }));
    }
    drop(tx);
    let mut samples: Vec<Sample> = Vec::new();
    while let Some(s) = rx.recv().await {
        samples.push(s);
    }
    for t in tasks {
        let _ = t.await;
    }
    // a run must not leave its arenas behind: the node would keep ticking them forever
    for a in &arenas {
        let _ = http
            .delete(format!("{url}/v1/server/worlds/{}", a.name))
            .header("x-musubi-server-key", KEY)
            .send()
            .await;
    }
    let secs = o.duration.as_secs_f64();
    let mut report = json!({
        "url": url, "clients": o.clients, "duration_secs": secs,
        "world": if arenas.is_empty() { Value::Null } else { json!({
            "worlds": arenas.len(), "entities_per_world": o.entities, "radius": o.radius,
            "frames": totals.frames.load(Ordering::Relaxed),
            "frames_per_client_per_sec": totals.frames.load(Ordering::Relaxed) as f64 / secs / o.clients.max(1) as f64,
            "kbytes_per_sec": totals.bytes.load(Ordering::Relaxed) as f64 / secs / 1024.0,
            "entity_updates_per_sec": totals.entities.load(Ordering::Relaxed) as f64 / secs,
            "sim_moves_ok": totals.moves_ok.load(Ordering::Relaxed),
            "sim_moves_err": totals.moves_err.load(Ordering::Relaxed),
            "sim_moves_err_timeout": totals.moves_timeout.load(Ordering::Relaxed),
            "sim_moves_err_4xx": totals.moves_4xx.load(Ordering::Relaxed),
            "sim_moves_err_5xx": totals.moves_5xx.load(Ordering::Relaxed),
            "sim_apply_mean_ms": totals.apply_ms_sum.load(Ordering::Relaxed) as f64
                / totals.apply_n.load(Ordering::Relaxed).max(1) as f64,
            "sim_apply_max_ms": totals.apply_ms_max.load(Ordering::Relaxed),
            "sim_ticks_per_sec": totals.sim_ticks.load(Ordering::Relaxed) as f64 / secs / arenas.len().max(1) as f64,
            "sim_skipped": totals.sim_skipped.load(Ordering::Relaxed),
            "client_err_connect": totals.err_connect.load(Ordering::Relaxed),
            "client_err_subscribe": totals.err_subscribe.load(Ordering::Relaxed),
            "client_err_stream": totals.err_stream.load(Ordering::Relaxed),
            "tick": scrape_tick(&http, &url, &format!("bench-{run_id}"), "musubi_world_tick_duration_seconds").await,
            "sims": o.sims.max(1),
            "stream_frames_per_sim_per_sec": totals.stream_frames.load(Ordering::Relaxed) as f64 / secs / (o.sims.max(1) * arenas.len().max(1)) as f64,
            "stream_reconnects": totals.stream_reconnects.load(Ordering::Relaxed),
            "handover_acks": totals.handover_acks.load(Ordering::Relaxed),
            "handovers_completed": scrape_counter(&http, &url, &format!("bench-{run_id}"), "musubi_world_handovers_total").await,
            "fanout": scrape_tick(&http, &url, &format!("bench-{run_id}"), "musubi_world_fanout_duration_seconds").await,
        }) },
        "auth": { "clients": o.clients, "total_ms": auth_ms, "per_client_ms": auth_ms / o.clients.max(1) as f64 },
        "scenarios": {}
    });
    for sc in &scenarios {
        let mut ok: Vec<f64> = samples
            .iter()
            .filter(|s| s.scenario == *sc && s.ok)
            .map(|s| s.ms)
            .collect();
        let throttled = samples
            .iter()
            .filter(|s| s.scenario == *sc && s.throttled)
            .count();
        let errors = samples
            .iter()
            .filter(|s| s.scenario == *sc && !s.ok && !s.throttled)
            .count();
        ok.sort_by(|a, b| a.partial_cmp(b).unwrap());
        report["scenarios"][*sc] = json!({
            "ops": ok.len(), "errors": errors, "throttled": throttled, "rps": ok.len() as f64 / secs,
            "p50_ms": percentile(&ok, 50.0), "p95_ms": percentile(&ok, 95.0),
            "p99_ms": percentile(&ok, 99.0), "max_ms": ok.last().copied().unwrap_or(0.0),
        });
    }
    if o.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{} clients, {:.0}s, auth {:.1} ms/client",
            o.clients,
            secs,
            report["auth"]["per_client_ms"].as_f64().unwrap_or(0.0)
        );
        println!(
            "{:<8} {:>8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>6} {:>7}",
            "scenario", "ops", "rps", "p50 ms", "p95 ms", "p99 ms", "max ms", "err", "429"
        );
        for sc in &scenarios {
            let r = &report["scenarios"][*sc];
            println!(
                "{:<8} {:>8} {:>7.0} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>6} {:>7}",
                sc,
                r["ops"],
                r["rps"].as_f64().unwrap_or(0.0),
                r["p50_ms"].as_f64().unwrap_or(0.0),
                r["p95_ms"].as_f64().unwrap_or(0.0),
                r["p99_ms"].as_f64().unwrap_or(0.0),
                r["max_ms"].as_f64().unwrap_or(0.0),
                r["errors"],
                r["throttled"]
            );
        }
        if report["scenarios"]
            .as_object()
            .is_some_and(|m| m.values().any(|v| v["throttled"].as_u64().unwrap_or(0) > 0))
        {
            println!("429 = per-account rate limit ([limits] in the server config); raise it or use more clients.");
        }
    }
    Ok(())
}

/// The player's link to the node: WebSocket (the default) or QUIC with the same `Hello` as the
/// native SDKs; keeps the tungstenite `Message` shape so the loop above is transport-agnostic.
enum Link {
    Ws(
        Box<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    Quic {
        conn: quinn::Connection,
        send: quinn::SendStream,
        /// Stream frames via a reader task: a `select!` over a half-read frame would lose bytes.
        frames: tokio::sync::mpsc::Receiver<Result<musubi_transport::Frame, String>>,
    },
}

type Msg = tokio_tungstenite::tungstenite::Message;

impl Link {
    async fn connect(
        transport: &str,
        url: &str,
        quic_addr: Option<&str>,
        token: &str,
        wire: &str,
    ) -> Result<Link, String> {
        use musubi_transport::quic::QuicConnection as Q;
        use musubi_transport::Frame;
        match transport {
            "quic" => {
                let addr: std::net::SocketAddr = quic_addr
                    .ok_or("--quic-addr required for --transport quic")?
                    .parse()
                    .map_err(|e| format!("quic addr: {e}"))?;
                let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
                    .map_err(|e| e.to_string())?;
                ep.set_default_client_config(
                    musubi_transport::quic::insecure_client_config().map_err(|e| e.to_string())?,
                );
                let conn = ep
                    .connect(addr, "localhost")
                    .map_err(|e| e.to_string())?
                    .await
                    .map_err(|e| e.to_string())?;
                let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
                let (ftx, frames) = tokio::sync::mpsc::channel(256);
                tokio::spawn(async move {
                    loop {
                        let r = Q::read_frame(&mut recv, 1 << 20)
                            .await
                            .map_err(|e| e.to_string());
                        let stop = r.is_err();
                        if ftx.send(r).await.is_err() || stop {
                            return;
                        }
                    }
                });
                Q::write_frame(
                    &mut send,
                    Frame::Text(serde_json::json!({"token": token, "format": wire}).to_string()),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(Link::Quic { conn, send, frames })
            }
            _ => {
                let (ws, _) = tokio_tungstenite::connect_async(format!(
                    "{}?token={token}&format={wire}",
                    ws_url(url)
                ))
                .await
                .map_err(|e| e.to_string())?;
                Ok(Link::Ws(Box::new(ws)))
            }
        }
    }

    async fn send(&mut self, m: Msg) -> Result<(), String> {
        use musubi_transport::quic::QuicConnection as Q;
        use musubi_transport::Frame;
        match self {
            Link::Ws(ws) => ws.send(m).await.map_err(|e| e.to_string()),
            Link::Quic { send, .. } => {
                let f = match m {
                    Msg::Text(t) => Frame::Text(t.to_string()),
                    Msg::Binary(b) => Frame::Binary(bytes::Bytes::from(b)),
                    _ => return Ok(()),
                };
                Q::write_frame(send, f).await.map_err(|e| e.to_string())
            }
        }
    }

    async fn next(&mut self) -> Option<Result<Msg, String>> {
        use musubi_transport::quic::QuicConnection as Q;
        use musubi_transport::Frame;
        match self {
            Link::Ws(ws) => ws.next().await.map(|r| r.map_err(|e| e.to_string())),
            Link::Quic { conn, frames, .. } => {
                let f = tokio::select! {
                    f = frames.recv() => f.unwrap_or(Err("stream closed".to_string())),
                    d = conn.read_datagram() => match d {
                        Ok(d) => Q::decode_datagram(d).map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    },
                };
                match f {
                    Ok(Frame::Text(t)) => Some(Ok(Msg::Text(t))),
                    Ok(Frame::Binary(b)) => Some(Ok(Msg::Binary(b.to_vec()))),
                    Ok(Frame::Close) => None,
                    Err(e) => Some(Err(e)),
                }
            }
        }
    }

    async fn close(&mut self, _reason: Option<()>) -> Result<(), String> {
        match self {
            Link::Ws(ws) => futures::SinkExt::close(ws.as_mut())
                .await
                .map_err(|e| e.to_string()),
            Link::Quic { conn, .. } => {
                conn.close(0u32.into(), b"bye");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert_eq!(percentile(&v, 50.0), 51.0);
        assert_eq!(percentile(&v, 99.0), 99.0);
        assert_eq!(percentile(&v, 100.0), 100.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(
            ws_url("https://api.example.com/"),
            "wss://api.example.com/ws"
        );
        assert_eq!(ws_url("http://127.0.0.1:7350"), "ws://127.0.0.1:7350/ws");
    }
}
