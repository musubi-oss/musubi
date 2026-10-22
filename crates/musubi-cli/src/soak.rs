//! `musubi soak`: a chaos stand so failover claims are measured, not asserted.
//!
//! Spawns `nodes` real `musubi-server` processes against one Postgres (same conventions as the
//! process tests), waits for Raft quorum, homes `worlds` persistent worlds across them and puts
//! `clients` load on the cluster: half hold resumable WS sessions and ping, half write a storage
//! object through one node and read it back through another. Every `kill_every` a random node is
//! SIGKILLed (or drained via the admin API) and restarted; for each incident we measure how long
//! the survivors take to drop the node and how long until every world it owned is homed elsewhere.
//! The report also counts client errors, WS reconnects (resumed vs fresh) and lost writes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::bench::{auth, percentile, ping_text, ws_url};

const KEY: &str = "defaultkey";

pub struct Opts {
    pub database_url: String,
    pub server_bin: Option<PathBuf>,
    pub nodes: usize,
    pub clients: usize,
    pub worlds: usize,
    pub matches: usize,
    pub mesh_entities: usize,
    pub duration: Duration,
    pub kill_every: Duration,
    /// Wait this long before restarting a killed node (0 = at once).
    pub rejoin_delay: Duration,
    pub mode: String,
    /// random | leader: which node incidents hit (leader = the current Raft leader).
    pub victim: String,
    pub json: bool,
}

struct Node {
    name: String,
    http: u16,
    bus: u16,
    dir: PathBuf,
    id: String,
    child: Option<tokio::process::Child>,
}

impl Node {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http)
    }
}

#[derive(Default)]
struct Counters {
    ws_ok: AtomicU64,
    ws_err: AtomicU64,
    ws_resumed: AtomicU64,
    ws_fresh: AtomicU64,
    st_ok: AtomicU64,
    st_err: AtomicU64,
    st_lost: AtomicU64,
    mt_sent: AtomicU64,
    mt_echo: AtomicU64,
    mt_err: AtomicU64,
    mesh_moves_ok: AtomicU64,
    mesh_moves_err: AtomicU64,
    mesh_lost: AtomicU64,
    stream_frames: AtomicU64,
    stream_reconnects: AtomicU64,
}

/// A world whose cell (1,0) is hosted on another node than its home; `entities` live on the border.
#[derive(Clone)]
struct MeshWorld {
    name: String,
    sim_a: uuid::Uuid,
    sim_b: uuid::Uuid,
    entities: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct Incident {
    node: String,
    action: &'static str,
    was_leader: bool,
    worlds: usize,
    matches: usize,
    /// Survivors no longer list the node.
    detected_ms: f64,
    /// Every world the node owned is homed on a survivor.
    rehomed_ms: f64,
    /// Every authoritative match the node owned runs on a survivor again (NaN when it owned none).
    matches_rehomed_ms: f64,
    /// Matches that came back with a tick far below the one seen before the kill (journal lost).
    matches_reset: usize,
    /// Every entity of every split world is accounted for on a survivor again (NaN without mesh).
    mesh_recovered_ms: f64,
    /// The restarted process answers and is back in the member list.
    rejoined_ms: f64,
    timed_out: bool,
}

/// Which nodes are currently reachable; clients pick from here so a killed node does not count
/// against the error budget twice.
type Alive = Arc<RwLock<Vec<String>>>;

fn free_port(udp: bool) -> u16 {
    if udp {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    } else {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
}

fn spawn(bin: &PathBuf, n: &mut Node, db: &str, group: &str) -> anyhow::Result<()> {
    let child = tokio::process::Command::new(bin)
        .env("MUSUBI__DATABASE__URL", db)
        .env("MUSUBI__DATABASE__RUN_MIGRATIONS", "true")
        .env("MUSUBI__DATABASE__MAX_CONNECTIONS", "8")
        .env("MUSUBI__NODE__ALLOW_INSECURE_DEFAULTS", "true")
        .env("MUSUBI__NODE__NAME", &n.name)
        .env("MUSUBI__NODE__DATA_DIR", &n.dir)
        .env("MUSUBI__NODE__CLUSTER_GROUP", group)
        .env("MUSUBI__NODE__BUS_LISTEN", format!("127.0.0.1:{}", n.bus))
        .env("MUSUBI__HTTP__LISTEN", format!("127.0.0.1:{}", n.http))
        .env("MUSUBI__LIMITS__ENABLED", "false")
        .env(
            "MUSUBI__LOG__FILTER",
            std::env::var("MUSUBI_SOAK_NODE_LOG_LEVEL").unwrap_or_else(|_| "warn".into()),
        )
        .stdout(node_log(n))
        .stderr(node_log(n))
        .kill_on_drop(true)
        .spawn()?;
    n.child = Some(child);
    Ok(())
}

/// Node stdout/stderr go to `$MUSUBI_SOAK_NODE_LOG_DIR/<name>.log` when set (appended across restarts),
/// otherwise it is discarded.
fn freeze(child: &tokio::process::Child, sig: &str) {
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args([sig, &pid.to_string()])
            .status();
    }
}

fn node_log(n: &Node) -> Stdio {
    match std::env::var("MUSUBI_SOAK_NODE_LOG_DIR") {
        Ok(dir) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::path::Path::new(&dir).join(format!("{}.log", n.name)))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null()),
        Err(_) => Stdio::null(),
    }
}

async fn get(client: &reqwest::Client, url: &str, path: &str) -> Option<Value> {
    client
        .get(format!("{url}{path}"))
        .header("x-musubi-server-key", KEY)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

async fn wait_quorum(client: &reqwest::Client, nodes: &[Node], want: usize) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let mut leaders = Vec::new();
        for n in nodes {
            if let Some(v) = get(client, &n.url(), "/v1/server/cluster/raft").await {
                if v["voters"].as_array().map(|a| a.len()) == Some(want) {
                    if let Some(l) = v["leader"].as_u64() {
                        leaders.push(l);
                    }
                }
            }
        }
        if leaders.len() == want && leaders.iter().all(|l| *l == leaders[0]) {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "no quorum within 120s: {leaders:?}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn node_id(client: &reqwest::Client, n: &Node) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(v) = get(client, &n.url(), "/v1/server/cluster/nodes").await {
            if let Some(id) = v["self"].as_str() {
                return Ok(id.to_string());
            }
        }
        anyhow::ensure!(Instant::now() < deadline, "node {} never answered", n.name);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn member_ids(client: &reqwest::Client, n: &Node) -> Option<Vec<String>> {
    let v = get(client, &n.url(), "/v1/server/cluster/nodes").await?;
    Some(
        v["nodes"]
            .as_array()?
            .iter()
            .filter(|m| m["state"] != "dead")
            .filter_map(|m| m["id"].as_str().map(str::to_string))
            .collect(),
    )
}

async fn world_home(client: &reqwest::Client, n: &Node, world: &str) -> Option<String> {
    let v = get(
        client,
        &n.url(),
        &format!("/v1/server/cluster/directory/world:{world}"),
    )
    .await?;
    v["value"]["node_id"].as_str().map(str::to_string)
}

/// match id → (home node id, snapshot tick) as the cluster sees it.
async fn cluster_matches(
    client: &reqwest::Client,
    n: &Node,
) -> Option<HashMap<String, (String, u64)>> {
    let v = get(client, &n.url(), "/v1/admin/cluster/matches").await?;
    Some(
        v["matches"]
            .as_array()?
            .iter()
            .filter_map(|m| {
                Some((
                    m["match_id"].as_str()?.to_string(),
                    (
                        m["node_id"].as_str().unwrap_or("").to_string(),
                        m["tick"].as_u64().unwrap_or(0),
                    ),
                ))
            })
            .collect(),
    )
}

/// Live tick of a match from the node that hosts it.
async fn match_tick(client: &reqwest::Client, url: &str, id: &str) -> Option<u64> {
    get(client, url, &format!("/v1/server/matches/{id}")).await?["tick"].as_u64()
}

/// Shared handles every match client needs.
struct MatchClientEnv {
    http: reqwest::Client,
    alive: Alive,
    node_urls: Arc<HashMap<String, String>>,
    c: Arc<Counters>,
    stop: Arc<AtomicBool>,
}

async fn match_client(env: MatchClientEnv, token: String, match_id: String, deadline: Instant) {
    let MatchClientEnv {
        http,
        alive,
        node_urls,
        c,
        stop,
    } = env;
    let mut rng = Rng::new();
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        // connect to whichever node hosts the match right now
        let Some(probe) = pick_alive(&alive, &mut rng).await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let home = match get(&http, &probe, "/v1/admin/cluster/matches").await {
            Some(v) => v["matches"]
                .as_array()
                .and_then(|a| a.iter().find(|m| m["match_id"] == match_id.as_str()))
                .and_then(|m| m["node_id"].as_str())
                .and_then(|id| node_urls.get(id).cloned()),
            None => None,
        };
        let home = match home {
            Some(h) if alive.read().await.contains(&h) => h,
            _ => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
        };
        let target = format!("{}?token={token}&format=json", ws_url(&home));
        let Ok(Ok((mut ws, _))) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(&target),
        )
        .await
        else {
            c.mt_err.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let join = json!({"cid": "j", "msg": {"match_join": {"match_id": match_id}}});
        if ws
            .send(tokio_tungstenite::tungstenite::Message::Text(
                join.to_string(),
            ))
            .await
            .is_err()
        {
            c.mt_err.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let mut n: u64 = 0;
        loop {
            if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                let _ = ws.close(None).await;
                return;
            }
            n += 1;
            let data = json!({"cid": null, "msg": {"match_data_send": {
                "match_id": match_id, "op_code": 1, "data": n.to_le_bytes().to_vec()}}});
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    data.to_string(),
                ))
                .await
                .is_err()
            {
                c.mt_err.fetch_add(1, Ordering::Relaxed);
                break;
            }
            c.mt_sent.fetch_add(1, Ordering::Relaxed);
            match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt)))) => {
                    if txt.contains("\"error\"") {
                        c.mt_err.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    c.mt_echo.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(Ok(_))) => {}
                _ => {
                    c.mt_err.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Tiny xorshift so the stand needs no extra dependency; seeded from the clock.
struct Rng(u64);
impl Rng {
    fn new() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self(t | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn find_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    match v {
        Value::Object(m) => {
            if let Some(s) = m.get(key).and_then(Value::as_str) {
                return Some(s);
            }
            m.values().find_map(|v| find_str(v, key))
        }
        Value::Array(a) => a.iter().find_map(|v| find_str(v, key)),
        _ => None,
    }
}

async fn pick_alive(alive: &Alive, rng: &mut Rng) -> Option<String> {
    let a = alive.read().await;
    if a.is_empty() {
        None
    } else {
        Some(a[rng.below(a.len())].clone())
    }
}

async fn ws_client(
    alive: Alive,
    token: String,
    deadline: Instant,
    c: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let mut rng = Rng::new();
    let ping = ping_text();
    let mut resume: Option<(String, String)> = None; // (node url, token)
    let mut resume_attempts = 0u32;
    // a real client backs off and retries: after a kill the holder is only declared dead by SWIM
    // a few seconds later, and until then the cluster refuses to hand the session to another node
    const RESUME_RETRIES: u32 = 8;
    const RESUME_PAUSE: Duration = Duration::from_millis(1500);
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let Some(url) = pick_alive(&alive, &mut rng).await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let pending = resume.clone();
        let (target, resumed) = match resume.take() {
            Some((u, t)) => (format!("{}?resume={t}&format=json", ws_url(&u)), true),
            None => (
                format!("{}?token={token}&format=json&resumable=true", ws_url(&url)),
                false,
            ),
        };
        let Ok((mut ws, _)) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(&target),
        )
        .await
        .map_err(|_| ())
        .and_then(|r| r.map_err(|_| ())) else {
            if resumed {
                if std::env::var_os("MUSUBI_SOAK_DEBUG").is_some() {
                    eprintln!("[soak] resume connect failed (attempt {resume_attempts}) to {url}");
                }
                if resume_attempts < RESUME_RETRIES {
                    resume_attempts += 1;
                    resume = pending.map(|(_, t)| (url.clone(), t));
                    tokio::time::sleep(RESUME_PAUSE).await;
                } else {
                    c.ws_fresh.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                c.ws_err.fetch_add(1, Ordering::Relaxed);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        // first frame: `session {session_id, resume_token}` for resumable sessions
        let mut resume_token = None;
        if let Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt)))) =
            tokio::time::timeout(Duration::from_secs(3), ws.next()).await
        {
            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                if find_str(&v, "code").is_some() && resumed {
                    if std::env::var_os("MUSUBI_SOAK_DEBUG").is_some() {
                        eprintln!("[soak] resume rejected (attempt {resume_attempts}): {txt}");
                    }
                    // rejected: the holder may not be declared dead yet, retry before giving up
                    if resume_attempts < RESUME_RETRIES {
                        resume_attempts += 1;
                        resume = pending.map(|(_, t)| (url.clone(), t));
                        tokio::time::sleep(RESUME_PAUSE).await;
                    } else {
                        c.ws_fresh.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                resume_token = find_str(&v, "resume_token").map(str::to_string);
            }
        }
        if resumed {
            c.ws_resumed.fetch_add(1, Ordering::Relaxed);
        }
        resume_attempts = 0;
        let session_node = url.clone();
        loop {
            if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                let _ = ws.close(None).await;
                return;
            }
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(ping.clone()))
                .await
                .is_err()
            {
                c.ws_err.fetch_add(1, Ordering::Relaxed);
                break;
            }
            match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)))) => {
                    c.ws_ok.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(Ok(_))) => {}
                _ => {
                    c.ws_err.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // socket broke: try to resume on any live node (the session is parked where it lived, so
        // this only succeeds after a drain, not after SIGKILL — which is what we want to see).
        if let Some(t) = resume_token {
            let any = pick_alive(&alive, &mut rng).await.unwrap_or(session_node);
            resume = Some((any, t));
        } else {
            if std::env::var_os("MUSUBI_SOAK_DEBUG").is_some() {
                eprintln!("[soak] socket lost without a resume token (hello not seen)");
            }
            c.ws_fresh.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn storage_client(
    http: reqwest::Client,
    alive: Alive,
    token: String,
    key: String,
    deadline: Instant,
    c: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let mut rng = Rng::new();
    let mut n: u64 = 0;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let Some(w) = pick_alive(&alive, &mut rng).await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        n += 1;
        let written = http
            .post(format!("{w}/v1/storage/write"))
            .bearer_auth(&token)
            .timeout(Duration::from_secs(3))
            .json(&json!({"objects": [{"collection": "soak", "key": key, "value": {"n": n}}]}))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !written {
            c.st_err.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        // read back through a different node when possible
        let mut seen = None;
        for _ in 0..3 {
            let Some(r) = pick_alive(&alive, &mut rng).await else {
                break;
            };
            let got = http
                .post(format!("{r}/v1/storage/read"))
                .bearer_auth(&token)
                .timeout(Duration::from_secs(3))
                .json(&json!({"objects": [{"collection": "soak", "key": key}]}))
                .send()
                .await;
            match got {
                Ok(resp) if resp.status().is_success() => {
                    let v: Value = resp.json().await.unwrap_or(Value::Null);
                    seen = v["objects"][0]["value"]["n"].as_u64();
                    break;
                }
                _ => {
                    c.st_err.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        match seen {
            Some(m) if m >= n => c.st_ok.fetch_add(1, Ordering::Relaxed),
            Some(_) => c.st_lost.fetch_add(1, Ordering::Relaxed),
            None => 0,
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn run(o: Opts) -> anyhow::Result<()> {
    anyhow::ensure!(o.nodes >= 3, "need at least 3 nodes for a Raft quorum");
    anyhow::ensure!(
        matches!(o.mode.as_str(), "kill" | "drain" | "mixed" | "stall"),
        "mode must be kill | drain | mixed | stall"
    );
    let bin = match o.server_bin {
        Some(b) => b,
        None => {
            let mut p = std::env::current_exe()?;
            p.set_file_name("musubi-server");
            p
        }
    };
    anyhow::ensure!(
        bin.exists(),
        "musubi-server binary not found at {}",
        bin.display()
    );
    let client = reqwest::Client::builder().build()?;
    let group = format!("soak-{}", uuid::Uuid::now_v7().simple());
    let root = std::env::temp_dir().join(&group);
    std::fs::create_dir_all(&root)?;
    eprintln!("[soak] group {group}, data in {}", root.display());

    let mut nodes: Vec<Node> = (0..o.nodes)
        .map(|i| Node {
            name: format!("n{i}"),
            http: free_port(false),
            bus: free_port(true),
            dir: root.join(format!("n{i}")),
            id: String::new(),
            child: None,
        })
        .collect();
    for n in nodes.iter_mut() {
        spawn(&bin, n, &o.database_url, &group)?;
    }
    wait_quorum(&client, &nodes, o.nodes).await?;
    for n in nodes.iter_mut() {
        n.id = node_id(&client, n).await?;
    }
    eprintln!("[soak] {} nodes up with quorum", nodes.len());

    // worlds, round-robin across nodes
    let mut worlds: Vec<String> = Vec::new();
    for w in 0..o.worlds {
        let n = &nodes[w % nodes.len()];
        let name = format!("soak-w{w}-{}", uuid::Uuid::now_v7().simple());
        let res = client
            .post(format!("{}/v1/server/worlds", n.url()))
            .header("x-musubi-server-key", KEY)
            .json(&json!({ "name": name, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 2.0, "handover_timeout_ticks": 30}, "controller": {} }))
            .send()
            .await?;
        anyhow::ensure!(
            res.status().is_success(),
            "world create failed: {}",
            res.status()
        );
        worlds.push(name);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut homed = 0;
        for w in &worlds {
            if world_home(&client, &nodes[0], w).await.is_some() {
                homed += 1;
            }
        }
        if homed == worlds.len() {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "worlds never got a home");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mesh = if o.mesh_entities > 0 {
        mesh_setup(&client, &nodes, &worlds, o.mesh_entities).await?
    } else {
        Vec::new()
    };

    // authoritative matches, round-robin across nodes
    let mut matches: Vec<String> = Vec::new();
    for m in 0..o.matches {
        let n = &nodes[m % nodes.len()];
        let res: Value = client
            .post(format!("{}/v1/server/matches", n.url()))
            .header("x-musubi-server-key", KEY)
            .json(&json!({ "module": "echo", "params": { "tick_rate": 10, "empty_ticks": 1_000_000, "label": "soak" } }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        matches.push(
            res["match_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("match create: {res}"))?
                .to_string(),
        );
    }
    let node_urls: Arc<HashMap<String, String>> =
        Arc::new(nodes.iter().map(|n| (n.id.clone(), n.url())).collect());

    // clients
    let alive: Alive = Arc::new(RwLock::new(nodes.iter().map(Node::url).collect()));
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let run_deadline = Instant::now() + o.duration;
    let mut tasks = Vec::new();
    for i in 0..o.clients {
        let url = nodes[i % nodes.len()].url();
        let token = auth(&client, &url, &format!("soak-{group}-{i}")).await?;
        let (alive, c, stop) = (alive.clone(), counters.clone(), stop.clone());
        if !matches.is_empty() && i % 3 == 2 {
            tasks.push(tokio::spawn(match_client(
                MatchClientEnv {
                    http: client.clone(),
                    alive,
                    node_urls: node_urls.clone(),
                    c,
                    stop,
                },
                token,
                matches[i % matches.len()].clone(),
                run_deadline,
            )));
        } else if i % 2 == 0 {
            tasks.push(tokio::spawn(ws_client(alive, token, run_deadline, c, stop)));
        } else {
            tasks.push(tokio::spawn(storage_client(
                client.clone(),
                alive,
                token,
                format!("c{i}"),
                run_deadline,
                c,
                stop,
            )));
        }
    }
    for mw in &mesh {
        let (alive, c, stop) = (alive.clone(), counters.clone(), stop.clone());
        tasks.push(tokio::spawn(mesh_mover(
            client.clone(),
            alive.clone(),
            mw.clone(),
            run_deadline,
            c.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(mesh_streamer(
            alive.clone(),
            mw.clone(),
            run_deadline,
            c,
            stop.clone(),
        )));
        tasks.push(tokio::spawn(mesh_leaser(
            client.clone(),
            alive,
            mw.clone(),
            run_deadline,
            stop,
        )));
    }
    eprintln!("[soak] {} clients running for {:?}", o.clients, o.duration);

    // chaos loop
    let mut rng = Rng::new();
    let mut incidents: Vec<Incident> = Vec::new();
    let mut toggle = false;
    while Instant::now() + o.kill_every < run_deadline {
        tokio::time::sleep(o.kill_every).await;
        if Instant::now() >= run_deadline {
            break;
        }
        let mut victim = rng.below(nodes.len());
        if o.victim == "leader" {
            for (i, n) in nodes.iter().enumerate() {
                let is_leader = get(&client, &n.url(), "/v1/server/cluster/raft")
                    .await
                    .map(|v| v["is_leader"] == true)
                    .unwrap_or(false);
                if is_leader {
                    victim = i;
                    break;
                }
            }
        }
        let action: &'static str = match o.mode.as_str() {
            "kill" => "kill",
            "drain" => "drain",
            "stall" => "stall",
            _ => {
                toggle = !toggle;
                if toggle {
                    "kill"
                } else {
                    "drain"
                }
            }
        };
        let vid = nodes[victim].id.clone();
        let was_leader = get(&client, &nodes[victim].url(), "/v1/server/cluster/raft")
            .await
            .map(|v| v["is_leader"] == true)
            .unwrap_or(false);
        let mut owned = Vec::new();
        for w in &worlds {
            if world_home(&client, &nodes[victim], w).await.as_deref() == Some(vid.as_str()) {
                owned.push(w.clone());
            }
        }
        // matches the victim hosts and the tick each had reached
        let mut owned_matches: Vec<(String, u64)> = Vec::new();
        if let Some(cm) = cluster_matches(&client, &nodes[victim]).await {
            for (id, (home, snap_tick)) in cm {
                if home == vid {
                    let tick = match_tick(&client, &nodes[victim].url(), &id)
                        .await
                        .unwrap_or(snap_tick);
                    owned_matches.push((id, tick));
                }
            }
        }
        alive.write().await.retain(|u| *u != nodes[victim].url());
        let survivor = (victim + 1) % nodes.len();
        let t0 = Instant::now();
        let mut child = nodes[victim].child.take().unwrap();
        if action == "kill" {
            let _ = child.kill().await;
        } else if action == "stall" {
            // a frozen process: SIGSTOP until the survivors have recovered, then SIGCONT
            freeze(&child, "-STOP");
        } else {
            let res = client
                .post(format!(
                    "{}/v1/admin/cluster/nodes/{vid}/drain",
                    nodes[survivor].url()
                ))
                .header("x-musubi-server-key", KEY)
                .send()
                .await;
            if !res.map(|r| r.status().is_success()).unwrap_or(false) {
                let _ = child.kill().await;
            }
        }
        if action != "stall" {
            let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        }
        let budget = Duration::from_secs(60);
        let mut timed_out = false;
        // detected: every survivor stopped listing the victim
        let detected_ms = loop {
            let mut gone = 0;
            for (i, n) in nodes.iter().enumerate() {
                if i == victim {
                    continue;
                }
                if let Some(ids) = member_ids(&client, n).await {
                    if !ids.contains(&vid) {
                        gone += 1;
                    }
                }
            }
            if gone == nodes.len() - 1 {
                break t0.elapsed().as_secs_f64() * 1e3;
            }
            if t0.elapsed() > budget {
                timed_out = true;
                break f64::NAN;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let rehomed_ms = loop {
            let mut moved = 0;
            // a victim that came back with the same identity may legitimately keep its worlds
            let back = member_ids(&client, &nodes[victim]).await.is_some();
            for w in &owned {
                match world_home(&client, &nodes[survivor], w).await {
                    Some(h) if h != vid || back => moved += 1,
                    _ => {}
                }
            }
            if moved == owned.len() {
                break t0.elapsed().as_secs_f64() * 1e3;
            }
            if t0.elapsed() > budget {
                timed_out = true;
                for (i, n) in nodes.iter().enumerate() {
                    if i == victim {
                        continue;
                    }
                    let raft = get(&client, &n.url(), "/v1/server/cluster/raft").await;
                    let ws = get(&client, &n.url(), "/v1/admin/cluster/worlds").await;
                    let mems = get(&client, &n.url(), "/v1/server/cluster/nodes").await;
                    eprintln!(
                        "[soak] diag {}: raft={} worlds={} nodes={}",
                        n.name,
                        raft.unwrap_or(Value::Null),
                        ws.unwrap_or(Value::Null),
                        mems.unwrap_or(Value::Null)
                    );
                }
                break f64::NAN;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let mut matches_reset = 0usize;
        let matches_rehomed_ms = if owned_matches.is_empty() {
            f64::NAN
        } else {
            loop {
                let cm = cluster_matches(&client, &nodes[survivor])
                    .await
                    .unwrap_or_default();
                let back = member_ids(&client, &nodes[victim]).await.is_some();
                let moved: Vec<(&String, &u64, String)> = owned_matches
                    .iter()
                    .filter_map(|(id, t)| {
                        cm.get(id)
                            .filter(|(home, _)| *home != vid || back)
                            .map(|(home, _)| (id, t, home.clone()))
                    })
                    .collect();
                if moved.len() == owned_matches.len() {
                    for (id, before, home) in moved {
                        let now_tick = match node_urls.get(&home) {
                            Some(u) => match_tick(&client, u, id).await.unwrap_or(0),
                            None => 0,
                        };
                        if now_tick == 0 || now_tick + 10 < *before {
                            eprintln!(
                                "[soak] match {id} came back at tick {now_tick}, was {before} before the kill"
                            );
                            matches_reset += 1;
                        }
                    }
                    break t0.elapsed().as_secs_f64() * 1e3;
                }
                if t0.elapsed() > budget {
                    timed_out = true;
                    break f64::NAN;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        // the survivors must account for every entity before the victim comes back
        let mesh_recovered_ms = if mesh.is_empty() {
            f64::NAN
        } else {
            let expected = mesh.len() * o.mesh_entities;
            let mut last_report = t0;
            loop {
                let mut total = 0;
                for mw in &mesh {
                    total += mesh_count(&client, &alive, mw).await;
                }
                if total == expected {
                    break t0.elapsed().as_secs_f64() * 1e3;
                }
                // a long recovery is only debuggable with the per-world picture as it happens
                if last_report.elapsed() > Duration::from_secs(5) {
                    last_report = Instant::now();
                    eprintln!(
                        "[soak] mesh: {total} of {expected} entities after {:.1} s",
                        t0.elapsed().as_secs_f64()
                    );
                    for mw in &mesh {
                        let n = mesh_count(&client, &alive, mw).await;
                        if n != o.mesh_entities {
                            eprintln!(
                                "[soak] mesh: {} has {n}: {}",
                                mw.name,
                                mesh_detail(&client, &alive, mw).await
                            );
                        }
                    }
                }
                if t0.elapsed() > budget {
                    timed_out = true;
                    eprintln!("[soak] mesh: {total} of {expected} entities accounted for after the incident");
                    for mw in &mesh {
                        let n = mesh_count(&client, &alive, mw).await;
                        if n != o.mesh_entities {
                            eprintln!("[soak] mesh: {} has {n}", mw.name);
                            eprintln!(
                                "[soak] mesh detail {}: {}",
                                mw.name,
                                mesh_detail(&client, &alive, mw).await
                            );
                            let urls: Vec<String> = alive.read().await.clone();
                            for u in &urls {
                                for (x, y) in [(0, 0), (1, 0)] {
                                    let r = client
                                        .get(format!(
                                            "{u}/v1/server/worlds/{}/view/{x}/{y}",
                                            mw.name
                                        ))
                                        .header("x-musubi-server-key", KEY)
                                        .send()
                                        .await;
                                    match r {
                                        Ok(r) => {
                                            let st = r.status();
                                            let v: Value = r.json().await.unwrap_or(Value::Null);
                                            eprintln!(
                                                "[soak]   {u} view({x},{y}) -> {st} owned={} ghosts={}",
                                                v["owned"].as_array().map(|a| a.len()).unwrap_or(0),
                                                v["ghosts"].as_array().map(|a| a.len()).unwrap_or(0)
                                            );
                                        }
                                        Err(e) => eprintln!("[soak]   {u} view({x},{y}) -> {e}"),
                                    }
                                }
                            }
                        }
                    }
                    counters
                        .mesh_lost
                        .fetch_add(expected.saturating_sub(total) as u64, Ordering::Relaxed);
                    break f64::NAN;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        // restart on the same ports/data dir
        tokio::time::sleep(o.rejoin_delay).await;
        let t1 = Instant::now();
        if action == "stall" {
            freeze(&child, "-CONT");
            nodes[victim].child = Some(child);
        } else {
            spawn(&bin, &mut nodes[victim], &o.database_url, &group)?;
        }
        let rejoined_ms = match tokio::time::timeout(budget, async {
            let id = node_id(&client, &nodes[victim]).await?;
            loop {
                if let Some(ids) = member_ids(&client, &nodes[survivor]).await {
                    if ids.contains(&id) {
                        return anyhow::Ok(id);
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        {
            Ok(Ok(id)) => {
                nodes[victim].id = id;
                alive.write().await.push(nodes[victim].url());
                t1.elapsed().as_secs_f64() * 1e3
            }
            _ => {
                timed_out = true;
                f64::NAN
            }
        };
        let inc = Incident {
            node: nodes[victim].name.clone(),
            action,
            was_leader,
            worlds: owned.len(),
            matches: owned_matches.len(),
            detected_ms,
            rehomed_ms,
            matches_rehomed_ms,
            matches_reset,
            mesh_recovered_ms,
            rejoined_ms,
            timed_out,
        };
        eprintln!(
            "[soak] {} {} (leader={}, worlds={}, matches={}): detected {:.0} ms, rehomed {:.0} ms, matches {:.0} ms, rejoined {:.0} ms{}",
            inc.action,
            inc.node,
            inc.was_leader,
            inc.worlds,
            inc.matches,
            inc.detected_ms,
            inc.rehomed_ms,
            inc.matches_rehomed_ms,
            inc.rejoined_ms,
            if inc.timed_out { " TIMEOUT" } else { "" }
        );
        incidents.push(inc);
    }

    tokio::time::sleep_until(tokio::time::Instant::from_std(run_deadline)).await;
    stop.store(true, Ordering::Relaxed);
    for t in tasks {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    for n in nodes.iter_mut() {
        if let Some(mut c) = n.child.take() {
            let _ = c.kill().await;
        }
    }
    let _ = std::fs::remove_dir_all(&root);

    let stats = |f: fn(&Incident) -> f64| -> HashMap<&'static str, f64> {
        let mut v: Vec<f64> = incidents.iter().map(f).filter(|x| x.is_finite()).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut m = HashMap::new();
        if !v.is_empty() {
            m.insert("p50", percentile(&v, 50.0));
            m.insert("p99", percentile(&v, 99.0));
            m.insert("max", *v.last().unwrap());
        }
        m
    };
    let c = &counters;
    let report = json!({
        "nodes": o.nodes,
        "clients": o.clients,
        "worlds": o.worlds,
        "duration_secs": o.duration.as_secs(),
        "incidents": incidents,
        "detected_ms": stats(|i| i.detected_ms),
        "rehomed_ms": stats(|i| i.rehomed_ms),
        "matches_rehomed_ms": stats(|i| i.matches_rehomed_ms),
        "matches_reset": incidents.iter().map(|i| i.matches_reset).sum::<usize>(),
        "match_clients": {
            "sent": c.mt_sent.load(Ordering::Relaxed),
            "echoed": c.mt_echo.load(Ordering::Relaxed),
            "errors": c.mt_err.load(Ordering::Relaxed),
        },
        "rejoined_ms": stats(|i| i.rejoined_ms),
        "mesh_recovered_ms": stats(|i| i.mesh_recovered_ms),
        "mesh": {
            "worlds": mesh.len(),
            "entities_per_world": o.mesh_entities,
            "moves_ok": c.mesh_moves_ok.load(Ordering::Relaxed),
            "moves_err": c.mesh_moves_err.load(Ordering::Relaxed),
            "stream_frames": c.stream_frames.load(Ordering::Relaxed),
            "stream_reconnects": c.stream_reconnects.load(Ordering::Relaxed),
            "lost_entities": c.mesh_lost.load(Ordering::Relaxed),
        },
        "ws": {
            "pings_ok": c.ws_ok.load(Ordering::Relaxed),
            "errors": c.ws_err.load(Ordering::Relaxed),
            "resumed": c.ws_resumed.load(Ordering::Relaxed),
            "fresh_reconnects": c.ws_fresh.load(Ordering::Relaxed),
        },
        "storage": {
            "verified_writes": c.st_ok.load(Ordering::Relaxed),
            "errors": c.st_err.load(Ordering::Relaxed),
            "lost_writes": c.st_lost.load(Ordering::Relaxed),
        },
    });
    if o.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "soak: {} nodes, {} clients, {} worlds, {}s, {} incidents",
            o.nodes,
            o.clients,
            o.worlds,
            o.duration.as_secs(),
            report["incidents"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        for k in [
            "detected_ms",
            "rehomed_ms",
            "matches_rehomed_ms",
            "rejoined_ms",
            "mesh_recovered_ms",
        ] {
            let s = &report[k];
            println!(
                "  {k:<12} p50 {:>8.0}  p99 {:>8.0}  max {:>8.0}",
                s["p50"].as_f64().unwrap_or(f64::NAN),
                s["p99"].as_f64().unwrap_or(f64::NAN),
                s["max"].as_f64().unwrap_or(f64::NAN)
            );
        }
        println!(
            "  ws: pings ok {}  errors {}  resumed {}  fresh reconnects {}",
            report["ws"]["pings_ok"],
            report["ws"]["errors"],
            report["ws"]["resumed"],
            report["ws"]["fresh_reconnects"]
        );
        println!(
            "  storage: verified writes {}  errors {}  lost writes {}",
            report["storage"]["verified_writes"],
            report["storage"]["errors"],
            report["storage"]["lost_writes"]
        );
    }
    let lost = c.st_lost.load(Ordering::Relaxed);
    let mesh_lost = c.mesh_lost.load(Ordering::Relaxed);
    if !o.json && !mesh.is_empty() {
        println!(
            "  mesh: {} worlds x {} entities across two nodes, moves ok {}  errors {}  lost entities {}",
            mesh.len(),
            o.mesh_entities,
            report["mesh"]["moves_ok"],
            report["mesh"]["moves_err"],
            mesh_lost
        );
        println!(
            "  stream: tick frames {}  reconnects {}",
            c.stream_frames.load(Ordering::Relaxed),
            c.stream_reconnects.load(Ordering::Relaxed)
        );
    }
    let timeouts = incidents.iter().filter(|i| i.timed_out).count();
    let reset: usize = incidents.iter().map(|i| i.matches_reset).sum();
    if !o.json {
        println!(
            "  matches: sent {}  echoed {}  errors {}  reset after failover {}",
            report["match_clients"]["sent"],
            report["match_clients"]["echoed"],
            report["match_clients"]["errors"],
            reset
        );
    }
    anyhow::ensure!(
        lost == 0 && timeouts == 0 && reset == 0 && mesh_lost == 0,
        "soak failed: {lost} lost writes, {timeouts} incidents timed out, {reset} matches reset, {mesh_lost} entities lost"
    );
    Ok(())
}

/// Split every world across two nodes: cell (1,0) is assigned to the node after the home, one
/// simulator per side leases its cell, and `n` persistent entities are spawned on the border.
async fn mesh_setup(
    client: &reqwest::Client,
    nodes: &[Node],
    worlds: &[String],
    n: usize,
) -> anyhow::Result<Vec<MeshWorld>> {
    let mut out = Vec::new();
    for (i, w) in worlds.iter().enumerate() {
        let home = &nodes[i % nodes.len()];
        let host = &nodes[(i + 1) % nodes.len()];
        let res = client
            .post(format!("{}/v1/admin/worlds/{w}/cells/1/0/host", home.url()))
            .header("x-musubi-server-key", KEY)
            .json(&json!({ "node_id": host.id }))
            .send()
            .await?;
        anyhow::ensure!(
            res.status().is_success(),
            "cell assignment failed: {}",
            res.status()
        );
        let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        for (node, sim, cell) in [(home, sim_a, (0, 0)), (host, sim_b, (1, 0))] {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let res = client
                    .post(format!("{}/v1/server/worlds/{w}/lease", node.url()))
                    .header("x-musubi-server-key", KEY)
                    .json(&json!({ "sim": sim, "cells": [{"x": cell.0, "y": cell.1}] }))
                    .send()
                    .await;
                if res.map(|r| r.status().is_success()).unwrap_or(false) {
                    break;
                }
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "lease of {cell:?} in {w} never succeeded"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        let mut entities = Vec::new();
        for k in 0..n {
            let res: Value = client
                .post(format!("{}/v1/server/worlds/{w}/spawn", home.url()))
                .header("x-musubi-server-key", KEY)
                .json(&json!({ "sim": sim_a, "x": 90.0, "y": mesh_y(k), "components": {"k": k}, "persistent": true }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            entities.push(
                res["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("spawn without id: {res}"))?
                    .to_string(),
            );
        }
        out.push(MeshWorld {
            name: w.clone(),
            sim_a,
            sim_b,
            entities,
        });
    }
    eprintln!(
        "[soak] {} worlds split across two nodes each, {n} border entities per world",
        out.len()
    );
    Ok(out)
}

/// Shuttle one entity at a time across the border (x 90 <-> 110): the simulator that owns the
/// entity's current cell applies the move on whichever node holds that cell, so every successful
/// move is followed by a handover to the other node.
async fn mesh_mover(
    client: reqwest::Client,
    alive: Alive,
    mw: MeshWorld,
    deadline: Instant,
    c: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let n = mw.entities.len();
    let mut right = vec![false; n];
    let mut k = 0usize;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let i = k % n;
        k += 1;
        let target = if right[i] { 90.0 } else { 110.0 };
        let urls: Vec<String> = alive.read().await.clone();
        let mut ok = false;
        'outer: for sim in [mw.sim_a, mw.sim_b] {
            for u in &urls {
                let res = client
                    .post(format!("{u}/v1/server/worlds/{}/apply", mw.name))
                    .header("x-musubi-server-key", KEY)
                    .json(&json!({ "sim": sim, "entity": mw.entities[i], "x": target, "y": mesh_y(i) }))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                if res.map(|r| r.status().is_success()).unwrap_or(false) {
                    ok = true;
                    break 'outer;
                }
            }
        }
        if ok {
            right[i] = !right[i];
            c.mesh_moves_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            c.mesh_moves_err.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Hold the streaming simulator protocol open for `sim_a`'s cell (0,0) on whichever node hosts
/// it: a handshake refused with a redirect (shard elsewhere) or a `lease` reply carrying
/// `redirect` moves the stream to that node, a dropped socket (node killed or drained) reconnects
/// through the alive list. Moves still go over HTTP, so every counted frame is a pure push.
async fn mesh_streamer(
    alive: Alive,
    mw: MeshWorld,
    deadline: Instant,
    c: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let cell = json!({"x": 0, "y": 0});
    let mut next: Option<String> = None;
    let mut rot = 0usize;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let base = match next.take() {
            Some(u) => u,
            None => {
                let urls = alive.read().await.clone();
                if urls.is_empty() {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                rot += 1;
                urls[rot % urls.len()].clone()
            }
        };
        let ws_base = base.replacen("http", "ws", 1);
        let Ok(mut req) = format!(
            "{ws_base}/v1/server/worlds/{}/stream?sim={}&delta=1",
            mw.name, mw.sim_a
        )
        .into_client_request() else {
            continue;
        };
        req.headers_mut()
            .insert("x-musubi-server-key", KEY.parse().unwrap());
        let connected = tokio::time::timeout(
            Duration::from_secs(2),
            tokio_tungstenite::connect_async(req),
        )
        .await;
        let Ok(Ok((mut ws, _))) = connected else {
            // no shard here (redirect) or the node is gone: try the next one
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let lease = json!({"op": "lease", "cells": [cell.clone()]}).to_string();
        if ws.send(Message::Text(lease)).await.is_err() {
            continue;
        }
        while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return;
            }
            let Message::Text(t) = m else { continue };
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v["op"] == "lease" {
                if let Some(r) = v["redirect"].as_str() {
                    next = reqwest::Url::parse(r).ok().map(|u| {
                        format!(
                            "{}://{}:{}",
                            u.scheme(),
                            u.host_str().unwrap_or("127.0.0.1"),
                            u.port_or_known_default().unwrap_or(80)
                        )
                    });
                    break;
                }
                if v["error"].is_string() {
                    break;
                }
            } else if v["type"] == "tick" {
                c.stream_frames.fetch_add(1, Ordering::Relaxed);
                // the stream drains this simulator's events, so handover requests addressed
                // to it must be answered here or the entities stay in transit
                for ev in v["events"].as_array().cloned().unwrap_or_default() {
                    if ev["type"] == "handover_request" {
                        let ack =
                            json!({"op": "handover_ack", "sim": mw.sim_a, "entity": ev["entity"]})
                                .to_string();
                        if ws.send(Message::Text(ack)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        if !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            c.stream_reconnects.fetch_add(1, Ordering::Relaxed);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Keep both simulators' leases alive (and re-take them wherever the cell lives after a failover).
async fn mesh_leaser(
    client: reqwest::Client,
    alive: Alive,
    mw: MeshWorld,
    deadline: Instant,
    stop: Arc<AtomicBool>,
) {
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let urls: Vec<String> = alive.read().await.clone();
        for (sim, cell) in [(mw.sim_a, (0, 0)), (mw.sim_b, (1, 0))] {
            let mut last = String::new();
            let mut ok = false;
            for u in &urls {
                let res = client
                    .post(format!("{u}/v1/server/worlds/{}/lease", mw.name))
                    .header("x-musubi-server-key", KEY)
                    .json(&json!({ "sim": sim, "cells": [{"x": cell.0, "y": cell.1}] }))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        ok = true;
                        break;
                    }
                    Ok(r) => {
                        let st = r.status();
                        let body = r.text().await.unwrap_or_default();
                        last.push_str(&format!(" | {u} -> {st} {}", &body[..body.len().min(160)]));
                    }
                    Err(e) => last.push_str(&format!(" | {u} -> {e}")),
                }
            }
            if !ok && std::env::var_os("MUSUBI_SOAK_LEASE_DEBUG").is_some() {
                eprintln!(
                    "[soak] lease {:?} for {} by {sim} failed everywhere:{last}",
                    cell,
                    &mw.name[5..7]
                );
            }
        }
        // play the simulators: when both border cells end up on one node (after a failover)
        // handovers are local and wait for the target simulator's ack — without it entities
        // flap in transit, hidden from every view. Events live on the node holding the shard,
        // so ask every alive node.
        for (sim, _) in [(mw.sim_a, (0, 0)), (mw.sim_b, (1, 0))] {
            for u in &urls {
                let events = client
                    .get(format!("{u}/v1/server/worlds/{}/events?sim={sim}", mw.name))
                    .header("x-musubi-server-key", KEY)
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                let Ok(r) = events else { continue };
                let Ok(v) = r.json::<Value>().await else {
                    continue;
                };
                for ev in v["events"].as_array().cloned().unwrap_or_default() {
                    if ev["type"] == "handover_request" {
                        let _ = client
                            .post(format!("{u}/v1/server/worlds/{}/handover/ack", mw.name))
                            .header("x-musubi-server-key", KEY)
                            .json(&json!({ "sim": sim, "entity": ev["entity"] }))
                            .timeout(Duration::from_secs(2))
                            .send()
                            .await;
                    }
                }
            }
        }
    }
}

/// Entities owned in the two border cells, read from whichever alive node serves each cell.
async fn mesh_count(client: &reqwest::Client, alive: &Alive, mw: &MeshWorld) -> usize {
    let urls: Vec<String> = alive.read().await.clone();
    let mut total = 0;
    for (x, y) in [(0, 0), (1, 0)] {
        for u in &urls {
            let t = Instant::now();
            let res = client
                .get(format!("{u}/v1/server/worlds/{}/view/{x}/{y}", mw.name))
                .header("x-musubi-server-key", KEY)
                .timeout(Duration::from_secs(2))
                .send()
                .await;
            if t.elapsed() > Duration::from_millis(1000) {
                eprintln!(
                    "[soak] mesh: slow view {u} ({x},{y}) {} in {} ms",
                    match &res {
                        Ok(r) => r.status().to_string(),
                        Err(e) => format!("err:{e}"),
                    },
                    t.elapsed().as_millis()
                );
            }
            if let Ok(r) = res {
                if r.status().is_success() {
                    if let Ok(v) = r.json::<Value>().await {
                        total += v["owned"].as_array().map(|a| a.len()).unwrap_or(0);
                        break;
                    }
                }
            }
        }
    }
    total
}

/// Per-node, per-cell view status for a mismatching world (diagnostics only).
async fn mesh_detail(client: &reqwest::Client, alive: &Alive, mw: &MeshWorld) -> String {
    let urls: Vec<String> = alive.read().await.clone();
    let mut parts = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for u in urls.iter() {
        let mut cells = Vec::new();
        for (x, y) in [(0, 0), (1, 0)] {
            let res = client
                .get(format!("{u}/v1/server/worlds/{}/view/{x}/{y}", mw.name))
                .header("x-musubi-server-key", KEY)
                .timeout(Duration::from_secs(2))
                .send()
                .await;
            let d = match res {
                Ok(r) if r.status().is_success() => {
                    let owner = r.url().port().map(|p| p.to_string()).unwrap_or_default();
                    match r.json::<Value>().await {
                        Ok(v) => {
                            let owned = v["owned"].as_array().cloned().unwrap_or_default();
                            for e in &owned {
                                if let Some(id) = e["id"].as_str() {
                                    seen.insert(id.to_string());
                                }
                            }
                            format!("owned={}@{owner}", owned.len())
                        }
                        Err(_) => "badjson".into(),
                    }
                }
                Ok(r) => r.status().as_u16().to_string(),
                Err(e) => format!(
                    "err:{}",
                    if e.is_timeout() {
                        "timeout"
                    } else if e.is_redirect() {
                        "redirect"
                    } else if e.is_connect() {
                        "connect"
                    } else {
                        "other"
                    }
                ),
            };
            cells.push(format!("({x},{y})={d}"));
        }
        parts.push(format!(
            "[{}]: {}",
            u.rsplit(':').next().unwrap_or(""),
            cells.join(" ")
        ));
    }
    let missing: Vec<&str> = mw
        .entities
        .iter()
        .filter(|e| !seen.contains(*e))
        .map(|e| e.as_str())
        .collect();
    // redirect chain per node/cell (no auto-follow) + node id → advertise map
    let raw = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| client.clone());
    let mut hops = Vec::new();
    for u in &urls {
        for (x, y) in [(0, 0), (1, 0)] {
            let mut url = format!("{u}/v1/server/worlds/{}/view/{x}/{y}", mw.name);
            let mut chain = Vec::new();
            for _ in 0..4 {
                let res = raw
                    .get(&url)
                    .header("x-musubi-server-key", KEY)
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                match res {
                    Ok(r) => {
                        let st = r.status().as_u16();
                        let loc = r
                            .headers()
                            .get("location")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string());
                        chain.push(format!("{st}@{}", url.rsplit('/').nth(4).unwrap_or("")));
                        match loc {
                            Some(l) if (300..400).contains(&st) => url = l,
                            _ => break,
                        }
                    }
                    Err(e) => {
                        chain.push(format!("err:{e}").chars().take(40).collect());
                        break;
                    }
                }
            }
            hops.push(format!(
                "{}({x},{y}): {}",
                u.rsplit(':').next().unwrap_or(""),
                chain.join(" -> ")
            ));
        }
    }
    let mut nodes_map = String::new();
    if let Some(u) = urls.first() {
        if let Ok(r) = client
            .get(format!("{u}/v1/server/cluster/nodes"))
            .header("x-musubi-server-key", KEY)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if let Ok(v) = r.json::<Value>().await {
                let arr = v["nodes"].as_array().cloned().unwrap_or_default();
                nodes_map = arr
                    .iter()
                    .map(|n| {
                        format!(
                            "{}={}{}",
                            n["id"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(13)
                                .collect::<String>(),
                            n["advertise"].as_str().unwrap_or(""),
                            if n["alive"].as_bool() == Some(false) {
                                "(dead)"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
    }
    format!(
        "{} missing={:?}\n    hops: {}\n    nodes: {nodes_map}",
        parts.join(" | "),
        missing,
        hops.join(" ; ")
    )
}

/// Border entities stay inside the first row of cells (cell_size 100).
fn mesh_y(k: usize) -> f64 {
    1.0 + (k % 24) as f64 * 4.0
}
