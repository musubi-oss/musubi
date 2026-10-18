//! `musubi module …`, `musubi fleet …`, `musubi deploy`: drive a running cluster's admin API
//! from a shell or CI — publish WASM modules (straight or as a canary), promote / roll back,
//! create or update fleets and their builds from a file, or apply one manifest with both.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use musubi_core::config::Config;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Args, Clone)]
pub struct ApiArgs {
    /// Base URL of any node of the cluster.
    #[arg(long, env = "MUSUBI_URL", default_value = "http://127.0.0.1:7350")]
    pub url: String,
    /// Server key (default: `[auth] server_key` of the loaded config).
    #[arg(long, env = "MUSUBI_SERVER_KEY")]
    pub key: Option<String>,
}

#[derive(Subcommand)]
pub enum ModuleCmd {
    /// Upload a module and make it the active build cluster-wide.
    Publish {
        name: String,
        /// `.wasm` / `.wat` file.
        path: PathBuf,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Upload a module as a canary: `--percent` of RPC traffic (and `--accounts`) run on it.
    Canary {
        name: String,
        path: PathBuf,
        #[arg(long, default_value_t = 10)]
        percent: i32,
        /// Comma-separated account ids always routed to the canary.
        #[arg(long, default_value = "")]
        accounts: String,
        #[arg(long, default_value = "")]
        note: String,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Make the canary the active build (or drop it with `--drop`).
    Promote {
        name: String,
        #[arg(long)]
        drop: bool,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Re-activate an earlier version from the module's history.
    Rollback {
        name: String,
        version: i32,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Version history of a module (or every loaded module when no name is given).
    Versions {
        name: Option<String>,
        #[command(flatten)]
        api: ApiArgs,
    },
}

#[derive(Subcommand)]
pub enum FleetCmd {
    /// Create the fleet described in a TOML/JSON file or update an existing one of that name;
    /// a `[build]` section adds a build version (canary or active).
    Push {
        file: PathBuf,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Fleets with their ready/allocated counts.
    List {
        #[command(flatten)]
        api: ApiArgs,
    },
}

/// `musubi deploy` manifest: modules and fleets applied in order.
#[derive(Subcommand)]
pub enum SchemaCmd {
    /// Attach a storage collection contract (JSON Schema, migrations, indexes) from a TOML/JSON
    /// file with a `collection` field; re-running with the same version is a no-op.
    Push {
        file: PathBuf,
        #[command(flatten)]
        api: ApiArgs,
    },
    /// Collection contracts currently attached.
    List {
        #[command(flatten)]
        api: ApiArgs,
    },
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Manifest {
    /// Applied first: data contracts land before the code that relies on them.
    pub schemas: Vec<Value>,
    pub modules: Vec<ManifestModule>,
    pub fleets: Vec<Value>,
}

#[derive(Deserialize)]
pub struct ManifestModule {
    pub name: String,
    pub path: PathBuf,
    /// > 0 publishes as a canary with that share instead of activating at once.
    #[serde(default)]
    pub canary_percent: i32,
    #[serde(default)]
    pub note: String,
}

pub struct Api {
    base: String,
    key: String,
    client: reqwest::Client,
}

enum Body {
    None,
    Json(Value),
    Bytes(Vec<u8>),
}

impl Api {
    pub fn new(base: &str, key: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn from_args(cfg: &Config, a: &ApiArgs) -> Self {
        let key = a.key.clone().unwrap_or_else(|| cfg.http.server_key.clone());
        Self::new(&a.url, &key)
    }

    async fn call(&self, method: &str, path: &str, body: Body) -> anyhow::Result<Value> {
        let m = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut r = self
            .client
            .request(m, format!("{}{path}", self.base))
            .header("x-musubi-server-key", &self.key);
        r = match body {
            Body::None => r,
            Body::Json(v) => r.json(&v),
            Body::Bytes(b) => r.header("content-type", "application/octet-stream").body(b),
        };
        let resp = r.send().await?;
        let st = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !st.is_success() {
            anyhow::bail!("{method} {path}: {st} {v}");
        }
        Ok(v)
    }

    pub async fn module_publish(&self, name: &str, bytes: Vec<u8>) -> anyhow::Result<Value> {
        self.call(
            "PUT",
            &format!("/v1/admin/runtime/modules/{name}"),
            Body::Bytes(bytes),
        )
        .await
    }

    pub async fn module_canary(
        &self,
        name: &str,
        bytes: Vec<u8>,
        percent: i32,
        accounts: &str,
        note: &str,
    ) -> anyhow::Result<Value> {
        let q = format!(
            "?percent={percent}&accounts={}&note={}",
            urlencoding::encode(accounts),
            urlencoding::encode(note)
        );
        self.call(
            "POST",
            &format!("/v1/admin/runtime/modules/{name}/canary{q}"),
            Body::Bytes(bytes),
        )
        .await
    }

    pub async fn module_promote(&self, name: &str, drop: bool) -> anyhow::Result<Value> {
        if drop {
            self.call(
                "DELETE",
                &format!("/v1/admin/runtime/modules/{name}/canary"),
                Body::None,
            )
            .await
        } else {
            self.call(
                "POST",
                &format!("/v1/admin/runtime/modules/{name}/canary/promote"),
                Body::None,
            )
            .await
        }
    }

    pub async fn module_rollback(&self, name: &str, version: i32) -> anyhow::Result<Value> {
        self.call(
            "POST",
            &format!("/v1/admin/runtime/modules/{name}/versions/{version}/rollback"),
            Body::None,
        )
        .await
    }

    pub async fn module_versions(&self, name: Option<&str>) -> anyhow::Result<Value> {
        match name {
            Some(n) => {
                self.call(
                    "GET",
                    &format!("/v1/admin/runtime/modules/{n}/versions"),
                    Body::None,
                )
                .await
            }
            None => {
                self.call("GET", "/v1/admin/runtime/modules", Body::None)
                    .await
            }
        }
    }

    pub async fn fleets(&self) -> anyhow::Result<Value> {
        self.call("GET", "/v1/server/fleets", Body::None).await
    }

    /// Upsert a fleet by name; `build` (if present in `spec`) is added as a build version.
    pub async fn schemas(&self) -> anyhow::Result<Value> {
        self.call("GET", "/v1/admin/storage_schemas", Body::None)
            .await
    }

    pub async fn schema_push(&self, mut spec: Value) -> anyhow::Result<Value> {
        let collection = spec
            .as_object_mut()
            .and_then(|o| o.remove("collection"))
            .and_then(|c| c.as_str().map(str::to_string))
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow::anyhow!("schema file needs a `collection`"))?;
        self.call(
            "PUT",
            &format!("/v1/admin/storage_schemas/{collection}"),
            Body::Json(spec),
        )
        .await
    }

    pub async fn fleet_push(&self, mut spec: Value) -> anyhow::Result<Value> {
        let name = spec["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("fleet file needs a `name`"))?
            .to_string();
        let build = spec
            .as_object_mut()
            .and_then(|o| o.remove("build"))
            .filter(|b| !b.is_null());
        let existing = self.fleets().await?;
        let found = existing
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["name"] == name)
            .cloned();
        let (fleet, action) = match found {
            None => (
                self.call("POST", "/v1/server/fleets", Body::Json(spec.clone()))
                    .await?,
                "created",
            ),
            Some(f) => {
                let id = f["id"].as_str().unwrap_or_default().to_string();
                let mut scale = serde_json::Map::new();
                for k in ["buffer", "max_servers", "min_servers", "policy"] {
                    if !spec[k].is_null() {
                        scale.insert(k.to_string(), spec[k].clone());
                    }
                }
                let fleet = if scale.is_empty() {
                    f
                } else {
                    self.call(
                        "POST",
                        &format!("/v1/server/fleets/{id}/scale"),
                        Body::Json(Value::Object(scale)),
                    )
                    .await?
                };
                (fleet, "updated")
            }
        };
        let id = fleet["id"].as_str().unwrap_or_default().to_string();
        let mut out = json!({ "fleet": fleet, "action": action });
        if let Some(mut b) = build {
            if b["spec"].is_null() && !spec["spec"].is_null() {
                b["spec"] = spec["spec"].clone();
            }
            if b["version"].as_str().unwrap_or("").is_empty() {
                anyhow::bail!("fleet `build` needs a `version`");
            }
            out["build"] = self
                .call(
                    "POST",
                    &format!("/v1/server/fleets/{id}/builds"),
                    Body::Json(b),
                )
                .await?;
        }
        Ok(out)
    }

    pub async fn deploy(&self, m: Manifest, base_dir: &Path) -> anyhow::Result<Value> {
        let mut schemas = Vec::new();
        for sc in m.schemas {
            schemas.push(self.schema_push(sc).await?);
        }
        let mut modules = Vec::new();
        for md in m.modules {
            let path = if md.path.is_absolute() {
                md.path.clone()
            } else {
                base_dir.join(&md.path)
            };
            let bytes = std::fs::read(&path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
            let r = if md.canary_percent > 0 {
                self.module_canary(&md.name, bytes, md.canary_percent, "", &md.note)
                    .await?
            } else {
                self.module_publish(&md.name, bytes).await?
            };
            modules.push(json!({ "name": md.name, "result": r }));
        }
        let mut fleets = Vec::new();
        for f in m.fleets {
            fleets.push(self.fleet_push(f).await?);
        }
        Ok(json!({ "schemas": schemas, "modules": modules, "fleets": fleets }))
    }
}

/// Parse a TOML (default) or JSON (`.json`, or a leading `{`) file into a JSON value.
pub fn read_value(path: &Path) -> anyhow::Result<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let json_like = path.extension().and_then(|e| e.to_str()) == Some("json")
        || text.trim_start().starts_with('{');
    if json_like {
        Ok(serde_json::from_str(&text)?)
    } else {
        let t: toml::Value = toml::from_str(&text)?;
        Ok(serde_json::to_value(t)?)
    }
}

fn print(v: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

pub async fn module(cfg: &Config, cmd: ModuleCmd) -> anyhow::Result<()> {
    let out = match cmd {
        ModuleCmd::Publish { name, path, api } => {
            Api::from_args(cfg, &api)
                .module_publish(&name, std::fs::read(&path)?)
                .await?
        }
        ModuleCmd::Canary {
            name,
            path,
            percent,
            accounts,
            note,
            api,
        } => {
            Api::from_args(cfg, &api)
                .module_canary(&name, std::fs::read(&path)?, percent, &accounts, &note)
                .await?
        }
        ModuleCmd::Promote { name, drop, api } => {
            Api::from_args(cfg, &api)
                .module_promote(&name, drop)
                .await?
        }
        ModuleCmd::Rollback { name, version, api } => {
            Api::from_args(cfg, &api)
                .module_rollback(&name, version)
                .await?
        }
        ModuleCmd::Versions { name, api } => {
            Api::from_args(cfg, &api)
                .module_versions(name.as_deref())
                .await?
        }
    };
    print(&out)
}

pub async fn fleet(cfg: &Config, cmd: FleetCmd) -> anyhow::Result<()> {
    let out = match cmd {
        FleetCmd::Push { file, api } => {
            Api::from_args(cfg, &api)
                .fleet_push(read_value(&file)?)
                .await?
        }
        FleetCmd::List { api } => Api::from_args(cfg, &api).fleets().await?,
    };
    print(&out)
}

pub async fn schema(cfg: &Config, cmd: SchemaCmd) -> anyhow::Result<()> {
    let out = match cmd {
        SchemaCmd::Push { file, api } => {
            Api::from_args(cfg, &api)
                .schema_push(read_value(&file)?)
                .await?
        }
        SchemaCmd::List { api } => Api::from_args(cfg, &api).schemas().await?,
    };
    print(&out)
}

pub async fn deploy(cfg: &Config, file: PathBuf, api: ApiArgs) -> anyhow::Result<()> {
    let m: Manifest = serde_json::from_value(read_value(&file)?)?;
    let base = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let out = Api::from_args(cfg, &api).deploy(m, &base).await?;
    print(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as AxPath, Query, State};
    use axum::routing::{get, post, put};
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<(String, String, String)>>>;

    #[derive(Clone)]
    struct Mock {
        log: Log,
        fleets: Arc<Mutex<Vec<Value>>>,
    }

    async fn serve() -> (String, Mock) {
        let mock = Mock {
            log: Arc::new(Mutex::new(Vec::new())),
            fleets: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route(
                "/v1/admin/runtime/modules/{name}",
                put(
                    |State(m): State<Mock>,
                     AxPath(name): AxPath<String>,
                     headers: axum::http::HeaderMap,
                     body: axum::body::Bytes| async move {
                        let key = headers
                            .get("x-musubi-server-key")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        m.log.lock().unwrap().push((
                            "PUT".into(),
                            format!("/v1/admin/runtime/modules/{name}"),
                            format!("key={key} len={}", body.len()),
                        ));
                        Json(json!({ "name": name, "version": 1 }))
                    },
                ),
            )
            .route(
                "/v1/admin/runtime/modules/{name}/canary",
                post(
                    |State(m): State<Mock>,
                     AxPath(name): AxPath<String>,
                     Query(q): Query<HashMap<String, String>>,
                     body: axum::body::Bytes| async move {
                        m.log.lock().unwrap().push((
                            "POST".into(),
                            format!("/v1/admin/runtime/modules/{name}/canary"),
                            format!(
                                "percent={} accounts={} len={}",
                                q.get("percent").cloned().unwrap_or_default(),
                                q.get("accounts").cloned().unwrap_or_default(),
                                body.len()
                            ),
                        ));
                        Json(json!({ "name": name, "canary": true }))
                    },
                ),
            )
            .route(
                "/v1/admin/storage_schemas/{collection}",
                put(
                    |State(m): State<Mock>,
                     AxPath(collection): AxPath<String>,
                     body: axum::body::Bytes| async move {
                        let v: Value = serde_json::from_slice(&body).unwrap();
                        m.log.lock().unwrap().push((
                            "PUT".into(),
                            format!("/v1/admin/storage_schemas/{collection}"),
                            v.to_string(),
                        ));
                        Json(json!({ "collection": collection, "version": v["version"] }))
                    },
                ),
            )
            .route(
                "/v1/server/fleets",
                get(|State(m): State<Mock>| async move {
                    Json(Value::Array(m.fleets.lock().unwrap().clone()))
                })
                .post(|State(m): State<Mock>, Json(b): Json<Value>| async move {
                    let mut f = b.clone();
                    f["id"] = json!("11111111-1111-1111-1111-111111111111");
                    m.fleets.lock().unwrap().push(f.clone());
                    m.log
                        .lock()
                        .unwrap()
                        .push(("POST".into(), "/v1/server/fleets".into(), b.to_string()));
                    Json(f)
                }),
            )
            .route(
                "/v1/server/fleets/{id}/scale",
                post(
                    |State(m): State<Mock>, AxPath(id): AxPath<String>, Json(b): Json<Value>| async move {
                        m.log.lock().unwrap().push((
                            "POST".into(),
                            format!("/v1/server/fleets/{id}/scale"),
                            b.to_string(),
                        ));
                        Json(json!({ "id": id, "buffer": b["buffer"] }))
                    },
                ),
            )
            .route(
                "/v1/server/fleets/{id}/builds",
                post(
                    |State(m): State<Mock>, AxPath(id): AxPath<String>, Json(b): Json<Value>| async move {
                        m.log.lock().unwrap().push((
                            "POST".into(),
                            format!("/v1/server/fleets/{id}/builds"),
                            b.to_string(),
                        ));
                        Json(json!({ "fleet_id": id, "version": b["version"], "state": "active" }))
                    },
                ),
            )
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, mock)
    }

    #[tokio::test]
    async fn publish_canary_push_and_deploy_hit_the_admin_api() {
        let (base, mock) = serve().await;
        let api = Api::new(&base, "k1");

        let r = api.module_publish("game", b"\0asm".to_vec()).await.unwrap();
        assert_eq!(r["version"], 1);
        let r = api
            .module_canary("game", b"\0asm2".to_vec(), 25, "a,b", "try")
            .await
            .unwrap();
        assert_eq!(r["canary"], true);

        let dir = std::env::temp_dir().join(format!("musubi-deploy-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fleet.toml"),
            r#"
name = "eu-shooter"
provider = "docker"
region = "eu"
buffer = 2
max_servers = 10
port_start = 7000
port_end = 7100

[spec]
image = "ghcr.io/acme/shooter:1.2"

[build]
version = "1.2"
canary_percent = 0
"#,
        )
        .unwrap();
        let v = read_value(&dir.join("fleet.toml")).unwrap();
        let out = api.fleet_push(v.clone()).await.unwrap();
        assert_eq!(out["action"], "created", "{out}");
        assert_eq!(out["build"]["version"], "1.2");
        let out = api.fleet_push(v).await.unwrap();
        assert_eq!(out["action"], "updated", "{out}");

        std::fs::write(dir.join("game.wasm"), b"\0asm3").unwrap();
        std::fs::write(
            dir.join("musubi.deploy.toml"),
            r#"
[[schemas]]
collection = "profiles"
version = 2
[schemas.schema]
type = "object"
[[schemas.indexes]]
field = "score"
kind = "number"

[[modules]]
name = "game"
path = "game.wasm"

[[fleets]]
name = "eu-shooter"
provider = "docker"
region = "eu"
buffer = 3
max_servers = 10
port_start = 7000
port_end = 7100
"#,
        )
        .unwrap();
        let m: Manifest =
            serde_json::from_value(read_value(&dir.join("musubi.deploy.toml")).unwrap()).unwrap();
        let out = api.deploy(m, &dir).await.unwrap();
        assert_eq!(out["schemas"][0]["collection"], "profiles", "{out}");
        assert_eq!(out["schemas"][0]["version"], 2);
        assert_eq!(out["modules"][0]["result"]["version"], 1);
        assert_eq!(out["fleets"][0]["action"], "updated");

        let log = mock.log.lock().unwrap();
        let paths: Vec<String> = log.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
        assert_eq!(
            paths,
            vec![
                "PUT /v1/admin/runtime/modules/game",
                "POST /v1/admin/runtime/modules/game/canary",
                "POST /v1/server/fleets",
                "POST /v1/server/fleets/11111111-1111-1111-1111-111111111111/builds",
                "POST /v1/server/fleets/11111111-1111-1111-1111-111111111111/scale",
                "POST /v1/server/fleets/11111111-1111-1111-1111-111111111111/builds",
                "PUT /v1/admin/storage_schemas/profiles",
                "PUT /v1/admin/runtime/modules/game",
                "POST /v1/server/fleets/11111111-1111-1111-1111-111111111111/scale",
            ],
            "{log:?}"
        );
        let schema_body: Value = serde_json::from_str(&log[6].2).unwrap();
        assert_eq!(schema_body["version"], 2);
        assert_eq!(schema_body["schema"]["type"], "object");
        assert_eq!(schema_body["indexes"][0]["kind"], "number");
        assert!(schema_body.get("collection").is_none(), "{schema_body}");
        assert!(log[0].2.contains("key=k1 len=4"), "{:?}", log[0]);
        assert!(
            log[1].2.contains("percent=25 accounts=a,b len=5"),
            "{:?}",
            log[1]
        );
        assert!(
            log[2].2.contains("\"image\":\"ghcr.io/acme/shooter:1.2\"")
                && !log[2].2.contains("build"),
            "{:?}",
            log[2]
        );
        assert!(
            log[3].2.contains("\"version\":\"1.2\"") && log[3].2.contains("shooter:1.2"),
            "{:?}",
            log[3]
        );
        assert!(
            log[4].2.contains("\"buffer\":2") && !log[4].2.contains("provider"),
            "{:?}",
            log[4]
        );
        assert!(log[8].2.contains("\"buffer\":3"), "{:?}", log[8]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
