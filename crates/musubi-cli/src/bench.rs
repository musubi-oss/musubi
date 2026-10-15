//! `musubi bench`: a small load generator so latency claims are measured, not asserted.
//! Every client authenticates with a device id, then loops its scenario until the deadline;
//! latencies are reported as p50/p95/p99/max with throughput and error counts.

use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use musubi_proto::{ClientMessage, Envelope, ServerMessage};
use serde_json::{json, Value};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Opts {
    pub url: String,
    pub clients: usize,
    pub duration: Duration,
    pub scenario: String,
    pub json: bool,
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

fn ws_url(http: &str) -> String {
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

async fn auth(http: &reqwest::Client, url: &str, id: &str) -> anyhow::Result<String> {
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
    let ping = serde_json::to_string(&Envelope {
        cid: None,
        msg: ClientMessage::Ping,
    })
    .unwrap();
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
        other => anyhow::bail!("unknown scenario {other}; use http|storage|ws|all"),
    };
    let run_id = uuid::Uuid::now_v7().simple().to_string();
    let t0 = Instant::now();
    let mut tokens = Vec::with_capacity(o.clients);
    for i in 0..o.clients {
        tokens.push(auth(&http, &url, &format!("bench-{run_id}-{i}")).await?);
    }
    let auth_ms = t0.elapsed().as_secs_f64() * 1e3;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let deadline = Instant::now() + o.duration;
    let mut tasks = Vec::new();
    for (i, token) in tokens.into_iter().enumerate() {
        let sc = scenarios[i % scenarios.len()];
        let (http, url, tx) = (http.clone(), url.clone(), tx.clone());
        tasks.push(tokio::spawn(async move {
            match sc {
                "http" => http_loop(http, url, token, deadline, tx).await,
                "storage" => storage_loop(http, url, token, deadline, tx).await,
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
    let secs = o.duration.as_secs_f64();
    let mut report = json!({
        "url": url, "clients": o.clients, "duration_secs": secs,
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
