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
