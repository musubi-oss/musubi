//! Kubernetes / Agones providers against a mock API server: manifests, auth, polling, cleanup.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_fleet::kubernetes::{KubeConfig, KubernetesProvider, Mode};
use musubi_fleet::{Fleet, FleetSpec, Provider, StartRequest};
use serde_json::{json, Value};
use uuid::Uuid;

/// (method, namespace-or-path, authorization header, body)
type Call = (String, String, Option<String>, Value);

#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Call>>,
    gets: AtomicUsize,
    fail: AtomicBool,
}

type S = Arc<Mock>;

async fn create(
    State(m): State<S>,
    Path(ns): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    m.calls
        .lock()
        .unwrap()
        .push(("POST".into(), ns, auth, body.clone()));
    (StatusCode::CREATED, Json(body))
}

async fn get_pod(State(m): State<S>, Path((ns, name)): Path<(String, String)>) -> Json<Value> {
    m.calls
        .lock()
        .unwrap()
        .push(("GET".into(), format!("{ns}/{name}"), None, Value::Null));
    let n = m.gets.fetch_add(1, Ordering::SeqCst);
    if m.fail.load(Ordering::SeqCst) {
        return Json(json!({ "status": { "phase": "Failed" } }));
    }
    Json(if n == 0 {
        json!({ "status": { "phase": "Pending" } })
    } else {
        json!({ "status": { "phase": "Running", "hostIP": "10.0.0.5", "podIP": "10.244.1.7" } })
    })
}

async fn get_gs(State(m): State<S>, Path((ns, name)): Path<(String, String)>) -> Json<Value> {
    m.calls
        .lock()
        .unwrap()
        .push(("GET".into(), format!("{ns}/{name}"), None, Value::Null));
    let n = m.gets.fetch_add(1, Ordering::SeqCst);
    Json(if n == 0 {
        json!({ "status": { "state": "Scheduled" } })
    } else {
        json!({ "status": { "state": "Scheduled", "address": "203.0.113.9", "ports": [{ "name": "game", "port": 7312 }] } })
    })
}

async fn delete(State(m): State<S>, Path((ns, name)): Path<(String, String)>) -> StatusCode {
    m.calls
        .lock()
        .unwrap()
        .push(("DELETE".into(), format!("{ns}/{name}"), None, Value::Null));
    StatusCode::OK
}

async fn mock_server() -> (String, S) {
    let m: S = Arc::new(Mock::default());
    let app = Router::new()
        .route("/api/v1/namespaces/{ns}/pods", post(create))
        .route(
            "/api/v1/namespaces/{ns}/pods/{name}",
            get(get_pod).delete(delete),
        )
        .route(
            "/apis/agones.dev/v1/namespaces/{ns}/gameservers",
            post(create),
        )
        .route(
            "/apis/agones.dev/v1/namespaces/{ns}/gameservers/{name}",
            get(get_gs).delete(delete),
        )
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), m)
}

fn fleet() -> Fleet {
    Fleet {
        id: Uuid::now_v7(),
        name: "k8s".into(),
        provider: "kubernetes".into(),
        region: "eu".into(),
        spec: FleetSpec {
            command: vec!["--map".into(), "dust".into()],
            image: Some("ghcr.io/example/gs:1".into()),
            env: [("MODE".to_string(), "ranked".to_string())]
                .into_iter()
                .collect(),
            working_dir: None,
            vcpu: None,
        },
        buffer: 1,
        max_servers: 2,
        min_servers: 0,
        policy: Default::default(),
        port_start: 7777,
        port_end: 7800,
        created_at: time::OffsetDateTime::now_utc(),
    }
}

fn provider(url: &str, mode: Mode) -> KubernetesProvider {
    KubernetesProvider::new(
        KubeConfig {
            url: url.into(),
            token: Some("sa-token".into()),
            namespace: "games".into(),
            ca_cert_pem: None,
            insecure: false,
        },
        mode,
    )
    .unwrap()
    .with_timeouts(Duration::from_millis(10), Duration::from_secs(5))
}

#[tokio::test]
async fn pods_provider_creates_pod_polls_host_ip_and_deletes() {
    let (url, m) = mock_server().await;
    let p = provider(&url, Mode::Pods);
    let f = fleet();
    let id = Uuid::now_v7();
    let started = p
        .start(StartRequest {
            fleet: &f,
            server_id: id,
            port: 7777,
            api_url: "http://api.musubi:8080",
            server_key: "sk",
        })
        .await
        .unwrap();
    assert_eq!((started.address.as_str(), started.port), ("10.0.0.5", 7777));
    {
        let calls = m.calls.lock().unwrap();
        let (_, ns, auth, body) = &calls[0];
        assert_eq!(ns, "games");
        assert_eq!(auth.as_deref(), Some("Bearer sa-token"));
        assert_eq!(body["kind"], "Pod");
        assert_eq!(
            body["metadata"]["name"],
            format!("musubi-gs-{}", id.simple())
        );
        assert_eq!(body["metadata"]["labels"]["musubi.fleet"], f.id.to_string());
        let c = &body["spec"]["containers"][0];
        assert_eq!(c["image"], "ghcr.io/example/gs:1");
        assert_eq!(c["args"], json!(["--map", "dust"]));
        assert_eq!(c["ports"][0]["hostPort"], 7777);
        let env: Vec<(String, String)> = c["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap().into(),
                    e["value"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert!(env.contains(&("MUSUBI_SERVER_ID".into(), id.to_string())));
        assert!(env.contains(&("MUSUBI_API".into(), "http://api.musubi:8080".into())));
        assert!(env.contains(&("MODE".into(), "ranked".into())));
        assert_eq!(
            calls.iter().filter(|c| c.0 == "GET").count(),
            2,
            "pending, then running"
        );
    }
    p.stop(id).await;
    let calls = m.calls.lock().unwrap();
    let last = calls.last().unwrap();
    assert_eq!(last.0, "DELETE");
    assert_eq!(last.1, format!("games/musubi-gs-{}", id.simple()));
}

#[tokio::test]
async fn pods_provider_cleans_up_a_failed_pod() {
    let (url, m) = mock_server().await;
    m.fail.store(true, Ordering::SeqCst);
    let p = provider(&url, Mode::Pods);
    let f = fleet();
    let id = Uuid::now_v7();
    let err = p
        .start(StartRequest {
            fleet: &f,
            server_id: id,
            port: 7777,
            api_url: "http://api",
            server_key: "sk",
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Failed"), "{err}");
    let calls = m.calls.lock().unwrap();
    assert!(calls.iter().any(|c| c.0 == "DELETE"), "failed pod deleted");
}

#[tokio::test]
async fn agones_provider_creates_gameserver_and_reads_dynamic_port() {
    let (url, m) = mock_server().await;
    let p = provider(&url, Mode::Agones);
    let mut f = fleet();
    f.provider = "agones".into();
    let id = Uuid::now_v7();
    let started = p
        .start(StartRequest {
            fleet: &f,
            server_id: id,
            port: 7777,
            api_url: "http://api",
            server_key: "sk",
        })
        .await
        .unwrap();
    assert_eq!(
        (started.address.as_str(), started.port),
        ("203.0.113.9", 7312)
    );
    let calls = m.calls.lock().unwrap();
    let body = &calls[0].3;
    assert_eq!(body["kind"], "GameServer");
    assert_eq!(body["apiVersion"], "agones.dev/v1");
    assert_eq!(body["spec"]["ports"][0]["containerPort"], 7777);
    assert_eq!(body["spec"]["ports"][0]["portPolicy"], "Dynamic");
    assert_eq!(
        body["spec"]["template"]["spec"]["containers"][0]["image"],
        "ghcr.io/example/gs:1"
    );
}
