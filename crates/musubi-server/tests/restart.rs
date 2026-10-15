//! Full-cluster restart with real processes: three `musubi-server` nodes with data dirs form a
//! Raft quorum, replicate a world's home, get SIGKILLed together, come back on the same ports
//! and data dirs — same node ids, a leader again, the directory entry still there from disk, and
//! the world re-adopted by its home.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;

const KEY: &str = "defaultkey";

struct Node {
    http: u16,
    bus: u16,
    dir: std::path::PathBuf,
    child: Option<tokio::process::Child>,
}

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

fn spawn(n: &mut Node, db: &str, group: &str, name: &str) {
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_musubi-server"))
        .env("MUSUBI__DATABASE__URL", db)
        .env("MUSUBI__DATABASE__RUN_MIGRATIONS", "true")
        .env("MUSUBI__DATABASE__MAX_CONNECTIONS", "4")
        .env("MUSUBI__NODE__ALLOW_INSECURE_DEFAULTS", "true")
        .env("MUSUBI__NODE__NAME", name)
        .env("MUSUBI__NODE__DATA_DIR", &n.dir)
        .env("MUSUBI__NODE__CLUSTER_GROUP", group)
        .env("MUSUBI__NODE__BUS_LISTEN", format!("127.0.0.1:{}", n.bus))
        .env("MUSUBI__HTTP__LISTEN", format!("127.0.0.1:{}", n.http))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn musubi-server");
    n.child = Some(child);
}

async fn get(client: &reqwest::Client, n: &Node, path: &str) -> Option<Value> {
    client
        .get(format!("http://127.0.0.1:{}{path}", n.http))
        .header("x-musubi-server-key", KEY)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

/// Wait until every node reports a leader and three voters; return the leader's raft id.
async fn wait_quorum(client: &reqwest::Client, nodes: &[Node]) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let mut leaders = Vec::new();
        for n in nodes {
            if let Some(v) = get(client, n, "/v1/server/cluster/raft").await {
                if v["voters"].as_array().map(|a| a.len()) == Some(3) {
                    if let Some(l) = v["leader"].as_u64() {
                        leaders.push(l);
                    }
                }
            }
        }
        if leaders.len() == 3 && leaders.iter().all(|l| *l == leaders[0]) {
            return leaders[0];
        }
        assert!(
            Instant::now() < deadline,
            "no quorum within 90s: {leaders:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn node_ids(client: &reqwest::Client, nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for n in nodes {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(v) = get(client, n, "/v1/server/cluster/nodes").await {
                if let Some(id) = v["self"].as_str() {
                    out.push(id.to_string());
                    break;
                }
            }
            assert!(Instant::now() < deadline, "node {} never answered", n.http);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    out
}

#[tokio::test]
async fn cluster_survives_full_restart_from_disk() {
    let Ok(db) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let group = format!("restart-{}", uuid::Uuid::now_v7().simple());
    let root = std::env::temp_dir().join(&group);
    let mut nodes: Vec<Node> = (0..3)
        .map(|i| Node {
            http: free_port(false),
            bus: free_port(true),
            dir: root.join(format!("n{i}")),
            child: None,
        })
        .collect();
    for (i, n) in nodes.iter_mut().enumerate() {
        spawn(n, &db, &group, &format!("n{i}"));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let leader1 = wait_quorum(&client, &nodes).await;
    let ids1 = node_ids(&client, &nodes).await;
    assert_eq!(
        ids1.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );

    // a world created on node 0 is published to the replicated directory (home = node 0)
    let world = format!("w-{}", uuid::Uuid::now_v7().simple());
    let res = client
        .post(format!(
            "http://127.0.0.1:{}/v1/server/worlds",
            nodes[0].http
        ))
        .header("x-musubi-server-key", KEY)
        .json(&serde_json::json!({ "name": world, "config": {}, "controller": {} }))
        .send()
        .await
        .unwrap();
    assert!(
        res.status().is_success(),
        "{}",
        res.text().await.unwrap_or_default()
    );
    let key = format!("world:{world}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let v = get(
            &client,
            &nodes[2],
            &format!("/v1/server/cluster/directory/{key}"),
        )
        .await;
        if v.as_ref().and_then(|v| v["value"]["node_id"].as_str()) == Some(ids1[0].as_str()) {
            break;
        }
        assert!(Instant::now() < deadline, "directory not replicated: {v:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // kill everything at once, then bring the same nodes back
    for n in nodes.iter_mut() {
        let mut c = n.child.take().unwrap();
        c.kill().await.unwrap();
        let _ = c.wait().await;
    }
    for (i, n) in nodes.iter_mut().enumerate() {
        spawn(n, &db, &group, &format!("n{i}"));
    }
    let leader2 = wait_quorum(&client, &nodes).await;
    let ids2 = node_ids(&client, &nodes).await;
    assert_eq!(ids1, ids2, "node ids come from data_dir");
    let _ = (leader1, leader2);

    // the directory came back from disk (nobody re-created the world) and node 0 adopted it
    let v = get(
        &client,
        &nodes[1],
        &format!("/v1/server/cluster/directory/{key}"),
    )
    .await
    .unwrap();
    assert_eq!(v["value"]["node_id"], ids1[0], "{v}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let v = get(&client, &nodes[0], "/v1/server/worlds").await;
        if v.as_ref()
            .map(|v| v.to_string().contains(&world))
            .unwrap_or(false)
        {
            break;
        }
        assert!(Instant::now() < deadline, "world not re-adopted: {v:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    for n in nodes.iter_mut() {
        if let Some(mut c) = n.child.take() {
            let _ = c.kill().await;
        }
    }
    let _ = std::fs::remove_dir_all(root);
}

/// SIGTERM to a node hosting a world: it disowns the world, announces its departure and exits
/// within the drain budget; the failover leader re-homes the world on a surviving node right away
/// (not after the 15 s dead-node timeout).
#[tokio::test]
async fn draining_node_hands_off_its_worlds() {
    let Ok(db) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let root = std::env::temp_dir().join(format!("musubi-drain-{}", uuid::Uuid::now_v7().simple()));
    let group = format!("drain-{}", uuid::Uuid::now_v7().simple());
    let mut nodes: Vec<Node> = (0..3)
        .map(|i| Node {
            http: free_port(false),
            bus: free_port(true),
            dir: root.join(format!("n{i}")),
            child: None,
        })
        .collect();
    for (i, n) in nodes.iter_mut().enumerate() {
        spawn(n, &db, &group, &format!("n{i}"));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    wait_quorum(&client, &nodes).await;
    let ids = node_ids(&client, &nodes).await;

    // pick a follower to drain so the remaining two keep a Raft leader
    let mut victim = 0;
    for (i, n) in nodes.iter().enumerate() {
        let r = get(&client, n, "/v1/server/cluster/raft").await.unwrap();
        if r["is_leader"] != true {
            victim = i;
            break;
        }
    }
    let other = (victim + 1) % 3;
    let world = format!("w-{}", uuid::Uuid::now_v7().simple());
    let res = client
        .post(format!(
            "http://127.0.0.1:{}/v1/server/worlds",
            nodes[victim].http
        ))
        .header("x-musubi-server-key", KEY)
        .json(&serde_json::json!({ "name": world, "config": {}, "controller": {} }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
    let key = format!("world:{world}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let v = get(
            &client,
            &nodes[other],
            &format!("/v1/server/cluster/directory/{key}"),
        )
        .await;
        if v.as_ref().and_then(|v| v["value"]["node_id"].as_str()) == Some(ids[victim].as_str()) {
            break;
        }
        assert!(Instant::now() < deadline, "directory not replicated: {v:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // re-check right before the signal: leadership may have moved meanwhile
    let r = get(&client, &nodes[victim], "/v1/server/cluster/raft")
        .await
        .unwrap();
    assert_ne!(
        r["is_leader"], true,
        "victim became leader; test picks followers"
    );
    let started = Instant::now();
    let mut child = nodes[victim].child.take().unwrap();
    let pid = child.id().unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(25), child.wait())
        .await
        .expect("drained node exits within the budget")
        .unwrap();
    assert!(status.success(), "{status:?}");

    // the world lives on another node now, well before the 15 s dead-node timeout
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let v = get(
            &client,
            &nodes[other],
            &format!("/v1/server/cluster/directory/{key}"),
        )
        .await;
        let home = v
            .as_ref()
            .and_then(|v| v["value"]["node_id"].as_str())
            .map(String::from);
        if let Some(h) = &home {
            if h != &ids[victim] && ids.contains(h) {
                break;
            }
        }
        assert!(Instant::now() < deadline, "world not re-homed: {v:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    let alive = get(&client, &nodes[other], "/v1/server/cluster/nodes")
        .await
        .unwrap();
    let n = alive["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 2, "{alive}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Scoped keys and bans are replicated over the node bus: issued/revoked on one process, they
/// take effect on another within a couple of seconds, without a restart.
#[tokio::test]
async fn keys_and_bans_replicate_across_processes() {
    let Ok(db) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let root = std::env::temp_dir().join(format!("musubi-repl-{}", uuid::Uuid::now_v7().simple()));
    let group = format!("repl-{}", uuid::Uuid::now_v7().simple());
    let mut nodes: Vec<Node> = (0..3)
        .map(|i| Node {
            http: free_port(false),
            bus: free_port(true),
            dir: root.join(format!("n{i}")),
            child: None,
        })
        .collect();
    for (i, n) in nodes.iter_mut().enumerate() {
        spawn(n, &db, &group, &format!("n{i}"));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    wait_quorum(&client, &nodes).await;
    let url = |n: &Node, path: &str| format!("http://127.0.0.1:{}{path}", n.http);

    // a key issued on node 0 opens /v1/server on node 1 ...
    let issued: Value = client
        .post(url(&nodes[0], "/v1/admin/keys"))
        .header("x-musubi-server-key", KEY)
        .json(&serde_json::json!({ "name": "repl", "role": "server" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let secret = issued["secret"].as_str().unwrap().to_string();
    let id = issued["id"].as_str().unwrap().to_string();
    let status_with = |n: usize, key: String| {
        let client = client.clone();
        let u = url(&nodes[n], "/v1/server/events");
        async move {
            client
                .get(u)
                .header("x-musubi-server-key", key)
                .send()
                .await
                .map(|r| r.status().as_u16())
                .unwrap_or(0)
        }
    };
    let until = |want: u16, n: usize, key: String| {
        let status_with = &status_with;
        async move {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let st = status_with(n, key.clone()).await;
                if st == want {
                    return st;
                }
                assert!(
                    Instant::now() < deadline,
                    "node {n} still answers {st}, wanted {want}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert_eq!(until(200, 1, secret.clone()).await, 200);
    assert_eq!(until(200, 2, secret.clone()).await, 200);
    // ... and is refused everywhere once revoked on node 0
    let st = client
        .delete(url(&nodes[0], &format!("/v1/admin/keys/{id}")))
        .header("x-musubi-server-key", KEY)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 200);
    assert_eq!(until(401, 1, secret.clone()).await, 401);
    assert_eq!(until(401, 2, secret.clone()).await, 401);

    // a ban issued on node 0 bites a live JWT on node 1
    let sess: Value = client
        .post(url(&nodes[1], "/v1/auth/device"))
        .json(&serde_json::json!({ "id": format!("repl-dev-{}", uuid::Uuid::now_v7().simple()) }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let me = |client: reqwest::Client, u: String, tok: String| async move {
        client
            .get(u)
            .bearer_auth(tok)
            .send()
            .await
            .map(|r| r.status().as_u16())
            .unwrap_or(0)
    };
    assert_eq!(
        me(client.clone(), url(&nodes[1], "/v1/account"), tok.clone()).await,
        200
    );
    let st = client
        .post(url(&nodes[0], &format!("/v1/admin/accounts/{acc}/ban")))
        .header("x-musubi-server-key", KEY)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 200);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let st = me(client.clone(), url(&nodes[1], "/v1/account"), tok.clone()).await;
        if st == 403 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "node 1 still answers {st} for a banned account"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    for n in nodes.iter_mut() {
        if let Some(mut c) = n.child.take() {
            let _ = c.kill().await;
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
