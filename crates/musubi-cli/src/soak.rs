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
    pub duration: Duration,
    pub kill_every: Duration,
    pub mode: String,
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
}

#[derive(Debug, serde::Serialize)]
struct Incident {
    node: String,
    action: &'static str,
    was_leader: bool,
    worlds: usize,
    /// Survivors no longer list the node.
    detected_ms: f64,
    /// Every world the node owned is homed on a survivor.
    rehomed_ms: f64,
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
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let Some(url) = pick_alive(&alive, &mut rng).await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
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
                c.ws_fresh.fetch_add(1, Ordering::Relaxed);
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
                    // resume rejected (parked state died with the node): fall back to a fresh session
                    c.ws_fresh.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                resume_token = find_str(&v, "resume_token").map(str::to_string);
            }
        }
        if resumed {
            c.ws_resumed.fetch_add(1, Ordering::Relaxed);
        }
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
        matches!(o.mode.as_str(), "kill" | "drain" | "mixed"),
        "mode must be kill | drain | mixed"
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
            .json(&json!({ "name": name, "config": {}, "controller": {} }))
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
        if i % 2 == 0 {
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
        let victim = rng.below(nodes.len());
        let action: &'static str = match o.mode.as_str() {
            "kill" => "kill",
            "drain" => "drain",
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
        alive.write().await.retain(|u| *u != nodes[victim].url());
        let survivor = (victim + 1) % nodes.len();
        let t0 = Instant::now();
        let mut child = nodes[victim].child.take().unwrap();
        if action == "kill" {
            let _ = child.kill().await;
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
        let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
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
            for w in &owned {
                match world_home(&client, &nodes[survivor], w).await {
                    Some(h) if h != vid => moved += 1,
                    _ => {}
                }
            }
            if moved == owned.len() {
                break t0.elapsed().as_secs_f64() * 1e3;
            }
            if t0.elapsed() > budget {
                timed_out = true;
                break f64::NAN;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        // restart on the same ports/data dir
        let t1 = Instant::now();
        spawn(&bin, &mut nodes[victim], &o.database_url, &group)?;
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
            detected_ms,
            rehomed_ms,
            rejoined_ms,
            timed_out,
        };
        eprintln!(
            "[soak] {} {} (leader={}, worlds={}): detected {:.0} ms, rehomed {:.0} ms, rejoined {:.0} ms{}",
            inc.action,
            inc.node,
            inc.was_leader,
            inc.worlds,
            inc.detected_ms,
            inc.rehomed_ms,
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
        "rejoined_ms": stats(|i| i.rejoined_ms),
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
        for k in ["detected_ms", "rehomed_ms", "rejoined_ms"] {
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
    let timeouts = incidents.iter().filter(|i| i.timed_out).count();
    anyhow::ensure!(
        lost == 0 && timeouts == 0,
        "soak failed: {lost} lost writes, {timeouts} incidents timed out"
    );
    Ok(())
}
