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
    pub radius: f64,
    /// Cap on deltas per second per world player (`None` = every tick).
    pub max_hz: Option<u32>,
    /// Wire encoding for the world players: `json` or `binary` (postcard).
    pub wire: String,
    pub json: bool,
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
    /// Why a player gave up: WS handshake, first subscribe, stream closed/error, resubscribe.
    err_connect: AtomicU64,
    err_subscribe: AtomicU64,
    err_stream: AtomicU64,
}

struct Arena {
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
    let sim = uuid::Uuid::now_v7();
    let n = (side / cell).ceil() as i64;
    let cells: Vec<Value> = (0..n)
        .flat_map(|x| (0..n).map(move |y| json!({"x": x, "y": y})))
        .collect();
    let r = http
        .post(format!("{url}/v1/server/worlds/{name}/lease"))
        .header("x-musubi-server-key", KEY)
        .json(&json!({"sim": sim, "cells": cells}))
        .send()
        .await?;
    anyhow::ensure!(r.status().is_success(), "lease: {}", r.status());
    let mut ids = Vec::with_capacity(entities);
    for i in 0..entities {
        let (x, y) = arena_point(side, i as u64);
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
        sim,
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
/// changes (a plain HTTP `apply` per entity — the simplest, not the fastest, simulator).
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
    loop {
        let window = *deadline.borrow_and_update();
        if window.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(33)).await;
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
        futures::future::join_all(futs).await;
        step += 1;
    }
}

/// First frames of a world run that are not deltas are printed (encoding/subscription problems).
static FRAME_DEBUG: AtomicU64 = AtomicU64::new(0);
static CONNECT_DEBUG: AtomicU64 = AtomicU64::new(0);

struct Player {
    url: String,
    wire: String,
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
    let (mut ws, _) = match tokio_tungstenite::connect_async(format!(
        "{}?token={token}&format={wire}",
        ws_url(&url)
    ))
    .await
    {
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
            Ok(_) => {
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

pub async fn run(o: Opts) -> anyhow::Result<()> {
    let url = o.url.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(o.clients)
        .build()?;
    let scenarios: Vec<&'static str> = match o.scenario.as_str() {
        "all" => vec!["http", "storage", "ws"],
        "http" => vec!["http"],
        "storage" => vec!["storage"],
        "ws" => vec!["ws"],
        "world" => vec!["world"],
        other => anyhow::bail!("unknown scenario {other}; use http|storage|ws|world|all"),
    };
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
            let arena = Arc::new(world_setup(&http, &url, &name, o.entities).await?);
            sims.push(tokio::spawn(world_sim(
                http.clone(),
                url.clone(),
                arena.clone(),
                window_rx.clone(),
                totals.clone(),
            )));
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
        let arena = arenas.get(i % arenas.len().max(1)).cloned();
        let (radius, totals) = (o.radius, totals.clone());
        let max_hz = o.max_hz;
        let wire = o.wire.clone();
        tasks.push(tokio::spawn(async move {
            match sc {
                "http" => http_loop(http, url, token, deadline, tx).await,
                "storage" => storage_loop(http, url, token, deadline, tx).await,
                "world" => {
                    world_loop(Player {
                        url,
                        wire,
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
            "client_err_connect": totals.err_connect.load(Ordering::Relaxed),
            "client_err_subscribe": totals.err_subscribe.load(Ordering::Relaxed),
            "client_err_stream": totals.err_stream.load(Ordering::Relaxed),
            "tick": scrape_tick(&http, &url, &format!("bench-{run_id}"), "musubi_world_tick_duration_seconds").await,
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
