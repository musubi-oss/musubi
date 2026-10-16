//! Dedicated-server orchestration v0: fleets describe what to run; the reconciler keeps `buffer`
//! servers ready (≤ `max_servers`) through a `Provider`; game servers talk back with an Agones-like
//! SDK surface (ready / health / allocate / shutdown / labels). Providers: `local` (child processes on this
//! node), `docker` (containers via the Docker CLI), `kubernetes` (pods) and `agones`
//! (GameServer CRs); edgegap/gamelift plug into the same trait.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub mod docker;
pub mod firecracker;
pub mod kubernetes;

pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetSpec {
    /// Executable + args for the `local` provider (or image for container providers).
    pub command: Vec<String>,
    pub image: Option<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fleet {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub region: String,
    pub spec: FleetSpec,
    pub buffer: i32,
    pub max_servers: i32,
    pub port_start: i32,
    pub port_end: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateFleet {
    pub name: String,
    pub provider: String,
    pub region: String,
    pub spec: FleetSpec,
    pub buffer: i32,
    pub max_servers: i32,
    pub port_start: i32,
    pub port_end: i32,
}

impl Default for CreateFleet {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: "local".into(),
            region: String::new(),
            spec: FleetSpec::default(),
            buffer: 0,
            max_servers: 1,
            port_start: 7777,
            port_end: 7877,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServer {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub state: String,
    pub node_id: Option<Uuid>,
    pub address: String,
    pub port: i32,
    pub pid: Option<i64>,
    pub labels: serde_json::Value,
    pub allocation: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub last_health: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// What a client gets back from an allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub server_id: Uuid,
    pub fleet_id: Uuid,
    pub address: String,
    pub port: i32,
    /// Token the game server validates through `/v1/gameserver/connect` (shared by the group).
    pub connect_token: String,
}

#[derive(Debug)]
pub struct Started {
    pub address: String,
    pub port: i32,
    pub pid: Option<i64>,
}

pub struct StartRequest<'a> {
    pub fleet: &'a Fleet,
    pub server_id: Uuid,
    pub port: i32,
    /// Base URL of this node's HTTP API and the server key, exposed to the game server.
    pub api_url: &'a str,
    pub server_key: &'a str,
}

/// How servers are started and stopped. Implementations must be cancel-safe and idempotent.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>>;
    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;
    /// `Some(false)` when the provider knows the process/container is gone.
    fn alive(&self, server_id: Uuid) -> Option<bool>;
}

/// Child processes on this node.
#[derive(Default)]
pub struct LocalProcessProvider {
    children: DashMap<Uuid, Arc<tokio::sync::Mutex<tokio::process::Child>>>,
    exited: DashMap<Uuid, i32>,
}

impl Provider for LocalProcessProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let Some((bin, args)) = req.fleet.spec.command.split_first() else {
                return Err(Error::InvalidArgument("fleet spec.command is empty".into()));
            };
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args)
                .envs(&req.fleet.spec.env)
                .env("MUSUBI_SERVER_ID", req.server_id.to_string())
                .env("MUSUBI_FLEET_ID", req.fleet.id.to_string())
                .env("MUSUBI_PORT", req.port.to_string())
                .env("MUSUBI_API", req.api_url)
                .env("MUSUBI_SERVER_KEY", req.server_key)
                .kill_on_drop(true)
                .stdin(std::process::Stdio::null());
            if let Some(d) = &req.fleet.spec.working_dir {
                cmd.current_dir(d);
            }
            let child = cmd
                .spawn()
                .map_err(|e| Error::Internal(format!("spawn {bin}: {e}")))?;
            let pid = child.id().map(|p| p as i64);
            let child = Arc::new(tokio::sync::Mutex::new(child));
            self.children.insert(req.server_id, child.clone());
            Ok(Started {
                address: "127.0.0.1".into(),
                port: req.port,
                pid,
            })
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some((_, child)) = self.children.remove(&server_id) {
                let mut c = child.lock().await;
                let _ = c.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            }
        })
    }

    fn alive(&self, server_id: Uuid) -> Option<bool> {
        let child = self.children.get(&server_id)?;
        let mut guard = child.try_lock().ok()?;
        match guard.try_wait() {
            Ok(Some(status)) => {
                self.exited.insert(server_id, status.code().unwrap_or(-1));
                Some(false)
            }
            Ok(None) => Some(true),
            Err(_) => None,
        }
    }
}

#[derive(Clone)]
pub struct FleetService {
    db: PgPool,
    node_id: Uuid,
    providers: Arc<HashMap<String, Arc<dyn Provider>>>,
    api_url: String,
    server_key: String,
}

fn row_to_fleet(
    r: (
        Uuid,
        String,
        String,
        String,
        serde_json::Value,
        i32,
        i32,
        i32,
        i32,
        OffsetDateTime,
    ),
) -> Fleet {
    Fleet {
        id: r.0,
        name: r.1,
        provider: r.2,
        region: r.3,
        spec: serde_json::from_value(r.4).unwrap_or_default(),
        buffer: r.5,
        max_servers: r.6,
        port_start: r.7,
        port_end: r.8,
        created_at: r.9,
    }
}

type ServerRow = (
    Uuid,
    Uuid,
    String,
    Option<Uuid>,
    String,
    i32,
    Option<i64>,
    serde_json::Value,
    Option<serde_json::Value>,
    OffsetDateTime,
    OffsetDateTime,
);

fn row_to_server(r: ServerRow) -> GameServer {
    GameServer {
        id: r.0,
        fleet_id: r.1,
        state: r.2,
        node_id: r.3,
        address: r.4,
        port: r.5,
        pid: r.6,
        labels: r.7,
        allocation: r.8,
        last_health: r.9,
        created_at: r.10,
    }
}

const FLEET_COLS: &str =
    "id, name, provider, region, spec, buffer, max_servers, port_start, port_end, created_at";
const SERVER_COLS: &str =
    "id, fleet_id, state, node_id, address, port, pid, labels, allocation, last_health, created_at";

impl FleetService {
    pub fn new(db: PgPool, node_id: Uuid, api_url: &str, server_key: &str) -> Self {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("local".into(), Arc::new(LocalProcessProvider::default()));
        providers.insert("docker".into(), Arc::new(docker::DockerProvider));
        Self {
            db,
            node_id,
            providers: Arc::new(providers),
            api_url: api_url.to_string(),
            server_key: server_key.to_string(),
        }
    }

    /// Register an extra provider under `name` (e.g. `kubernetes` / `agones` when configured).
    pub fn with_provider(mut self, name: &str, provider: Arc<dyn Provider>) -> Self {
        let mut m: HashMap<String, Arc<dyn Provider>> = (*self.providers).clone();
        m.insert(name.to_string(), provider);
        self.providers = Arc::new(m);
        self
    }

    pub fn providers(&self) -> Vec<&'static str> {
        self.providers.values().map(|p| p.name()).collect()
    }

    pub async fn create(&self, c: CreateFleet) -> Result<Fleet> {
        if c.name.is_empty() || c.name.len() > 64 {
            return Err(Error::InvalidArgument("fleet name 1..=64 chars".into()));
        }
        if !self.providers.contains_key(&c.provider) {
            return Err(Error::InvalidArgument(format!(
                "unknown provider {}",
                c.provider
            )));
        }
        if c.max_servers < 1 || c.buffer < 0 || c.buffer > c.max_servers {
            return Err(Error::InvalidArgument(
                "need 0 <= buffer <= max_servers, max_servers >= 1".into(),
            ));
        }
        if c.port_start < 1 || c.port_end < c.port_start || c.port_end > 65535 {
            return Err(Error::InvalidArgument("bad port range".into()));
        }
        let id = Uuid::now_v7();
        let r = sqlx::query_as::<_, (Uuid, String, String, String, serde_json::Value, i32, i32, i32, i32, OffsetDateTime)>(&format!(
            "insert into musubi.fleets (id, name, provider, region, spec, buffer, max_servers, port_start, port_end)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {FLEET_COLS}"
        ))
        .bind(id)
        .bind(&c.name)
        .bind(&c.provider)
        .bind(&c.region)
        .bind(serde_json::to_value(&c.spec)?)
        .bind(c.buffer)
        .bind(c.max_servers)
        .bind(c.port_start)
        .bind(c.port_end)
        .fetch_one(&self.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => Error::Conflict("fleet name taken".into()),
            e => Error::internal(e),
        })?;
        Ok(row_to_fleet(r))
    }

    pub async fn list(&self) -> Result<Vec<Fleet>> {
        let rows = sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets order by created_at"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_fleet).collect())
    }

    pub async fn get(&self, id: Uuid) -> Result<Fleet> {
        sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_fleet)
        .ok_or_else(|| Error::NotFound("fleet".into()))
    }

    pub async fn get_by_name(&self, name: &str) -> Result<Fleet> {
        sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets where name = $1"
        ))
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_fleet)
        .ok_or_else(|| Error::NotFound(format!("fleet {name}")))
    }

    /// Change buffer / max_servers; the reconciler follows.
    pub async fn scale(
        &self,
        id: Uuid,
        buffer: Option<i32>,
        max_servers: Option<i32>,
    ) -> Result<Fleet> {
        let f = self.get(id).await?;
        let buffer = buffer.unwrap_or(f.buffer);
        let max = max_servers.unwrap_or(f.max_servers);
        if max < 1 || buffer < 0 || buffer > max {
            return Err(Error::InvalidArgument(
                "need 0 <= buffer <= max_servers, max_servers >= 1".into(),
            ));
        }
        sqlx::query("update musubi.fleets set buffer = $2, max_servers = $3, updated_at = now() where id = $1")
            .bind(id)
            .bind(buffer)
            .bind(max)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.get(id).await
    }

    /// Stop every server of the fleet, then delete it.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let f = self.get(id).await?;
        for s in self.servers(id, None).await? {
            self.terminate(&f, s.id).await;
        }
        sqlx::query("delete from musubi.fleets where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn servers(&self, fleet_id: Uuid, state: Option<&str>) -> Result<Vec<GameServer>> {
        let rows = sqlx::query_as::<_, ServerRow>(&format!(
            "select {SERVER_COLS} from musubi.game_servers where fleet_id = $1 and ($2::text is null or state = $2) order by created_at"
        ))
        .bind(fleet_id)
        .bind(state)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_server).collect())
    }

    pub async fn server(&self, id: Uuid) -> Result<GameServer> {
        sqlx::query_as::<_, ServerRow>(&format!(
            "select {SERVER_COLS} from musubi.game_servers where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_server)
        .ok_or_else(|| Error::NotFound("game server".into()))
    }

    async fn set_state(&self, id: Uuid, from: &[&str], to: &str) -> Result<GameServer> {
        let froms: Vec<String> = from.iter().map(|s| s.to_string()).collect();
        let r = sqlx::query_as::<_, ServerRow>(&format!(
            "update musubi.game_servers set state = $2, updated_at = now(), last_health = now()
             where id = $1 and (cardinality($3::text[]) = 0 or state = any($3)) returning {SERVER_COLS}"
        ))
        .bind(id)
        .bind(to)
        .bind(&froms)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        match r {
            Some(r) => Ok(row_to_server(r)),
            None => {
                let cur = self.server(id).await?;
                Err(Error::Conflict(format!(
                    "server is {}, cannot move to {to}",
                    cur.state
                )))
            }
        }
    }

    // ----- SDK surface (called by the game server itself) -----

    pub async fn sdk_ready(&self, id: Uuid) -> Result<GameServer> {
        self.set_state(id, &["scheduled", "starting", "ready"], "ready")
            .await
    }

    pub async fn sdk_health(&self, id: Uuid) -> Result<()> {
        sqlx::query("update musubi.game_servers set last_health = now() where id = $1 and state not in ('terminated')")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Self-allocation (the server decided it is in use, e.g. a player connected directly).
    pub async fn sdk_allocate(&self, id: Uuid) -> Result<GameServer> {
        self.set_state(id, &["ready", "allocated"], "allocated")
            .await
    }

    pub async fn sdk_labels(&self, id: Uuid, labels: serde_json::Value) -> Result<GameServer> {
        if !labels.is_object() || labels.to_string().len() > 4096 {
            return Err(Error::InvalidArgument(
                "labels must be an object ≤ 4 KB".into(),
            ));
        }
        sqlx::query("update musubi.game_servers set labels = labels || $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(labels)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.server(id).await
    }

    /// Validate the allocation's connect token (shared by the whole group; the game server calls
    /// this when a client arrives). The first valid connect moves the server to `in_game`.
    pub async fn sdk_connect(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        let r: Option<Option<serde_json::Value>> = sqlx::query_scalar(
            "update musubi.game_servers set state = case when state = 'allocated' then 'in_game' else state end, updated_at = now()
             where id = $1 and connect_token = $2 and state in ('allocated', 'in_game') returning allocation",
        )
        .bind(id)
        .bind(token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        r.flatten()
            .ok_or_else(|| Error::PermissionDenied("bad connect token".into()))
    }

    pub async fn sdk_shutdown(&self, id: Uuid) -> Result<()> {
        let s = self.server(id).await?;
        let f = self.get(s.fleet_id).await?;
        self.terminate(&f, id).await;
        Ok(())
    }

    // ----- control surface -----

    /// Hand a ready server to a caller; `payload` travels to the game server with the connect token.
    pub async fn allocate(&self, fleet_id: Uuid, payload: serde_json::Value) -> Result<Allocation> {
        let token = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
        let alloc = serde_json::json!({ "payload": payload, "at": OffsetDateTime::now_utc().unix_timestamp() });
        let r = sqlx::query_as::<_, (Uuid, String, i32)>(
            "update musubi.game_servers set state = 'allocated', allocation = $2, connect_token = $3, updated_at = now()
             where id = (select id from musubi.game_servers where fleet_id = $1 and state = 'ready'
                         order by created_at limit 1 for update skip locked)
             returning id, address, port",
        )
        .bind(fleet_id)
        .bind(alloc)
        .bind(&token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::ResourceExhausted("no ready server in fleet".into()))?;
        Ok(Allocation {
            server_id: r.0,
            fleet_id,
            address: r.1,
            port: r.2,
            connect_token: token,
        })
    }

    pub async fn terminate(&self, fleet: &Fleet, id: Uuid) {
        if let Some(p) = self.providers.get(&fleet.provider) {
            p.stop(id).await;
        }
        let _ = sqlx::query(
            "update musubi.game_servers set state = 'terminated', updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await;
    }

    async fn free_port(&self, f: &Fleet) -> Result<i32> {
        let used: Vec<i32> = sqlx::query_scalar(
            "select port from musubi.game_servers where node_id = $1 and state <> 'terminated'",
        )
        .bind(self.node_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        (f.port_start..=f.port_end)
            .find(|p| !used.contains(p))
            .ok_or_else(|| {
                Error::ResourceExhausted("fleet port range exhausted on this node".into())
            })
    }

    /// Start one server of the fleet on this node.
    pub async fn start_one(&self, f: &Fleet) -> Result<GameServer> {
        let provider =
            self.providers.get(&f.provider).cloned().ok_or_else(|| {
                Error::InvalidArgument(format!("unknown provider {}", f.provider))
            })?;
        let id = Uuid::now_v7();
        let port = self.free_port(f).await?;
        sqlx::query("insert into musubi.game_servers (id, fleet_id, state, node_id, port) values ($1, $2, 'starting', $3, $4)")
            .bind(id)
            .bind(f.id)
            .bind(self.node_id)
            .bind(port)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        match provider
            .start(StartRequest {
                fleet: f,
                server_id: id,
                port,
                api_url: &self.api_url,
                server_key: &self.server_key,
            })
            .await
        {
            Ok(st) => {
                sqlx::query("update musubi.game_servers set address = $2, port = $3, pid = $4, updated_at = now() where id = $1")
                    .bind(id)
                    .bind(&st.address)
                    .bind(st.port)
                    .bind(st.pid)
                    .execute(&self.db)
                    .await
                    .map_err(Error::internal)?;
                self.server(id).await
            }
            Err(e) => {
                let _ = sqlx::query("update musubi.game_servers set state = 'terminated', labels = labels || jsonb_build_object('error', $2::text) where id = $1")
                    .bind(id)
                    .bind(e.to_string())
                    .execute(&self.db)
                    .await;
                Err(e)
            }
        }
    }

    /// One reconcile pass over every fleet: reap dead/unhealthy servers, then top up to `buffer`
    /// ready+starting servers without exceeding `max_servers`. Returns (started, reaped).
    pub async fn reconcile(&self) -> Result<(usize, usize)> {
        let mut started = 0;
        let mut reaped = 0;
        for f in self.list().await? {
            let provider = self.providers.get(&f.provider).cloned();
            let servers = self.servers(f.id, None).await?;
            let now = OffsetDateTime::now_utc();
            for s in servers.iter().filter(|s| s.state != "terminated") {
                let mine = s.node_id == Some(self.node_id);
                let dead_process =
                    mine && provider.as_ref().and_then(|p| p.alive(s.id)) == Some(false);
                let stale = now - s.last_health > HEALTH_TIMEOUT && s.state != "starting";
                let stuck_starting =
                    s.state == "starting" && now - s.created_at > Duration::from_secs(120);
                if dead_process || stale || stuck_starting {
                    tracing::warn!(server = %s.id, fleet = %f.name, state = %s.state, dead_process, stale, "reaping game server");
                    self.terminate(&f, s.id).await;
                    reaped += 1;
                }
            }
            let servers = self.servers(f.id, None).await?;
            let live = servers.iter().filter(|s| s.state != "terminated").count() as i32;
            let available = servers
                .iter()
                .filter(|s| s.state == "ready" || s.state == "starting" || s.state == "scheduled")
                .count() as i32;
            let want = (f.buffer - available).min(f.max_servers - live).max(0);
            for _ in 0..want {
                match self.start_one(&f).await {
                    Ok(_) => started += 1,
                    Err(e) => {
                        tracing::warn!(fleet = %f.name, error = %e, "could not start game server");
                        break;
                    }
                }
            }
        }
        Ok((started, reaped))
    }

    /// Background reconciler; `active` gates each pass (cluster-wide: only the lease holder runs it).
    pub fn spawn_reconciler(
        self: Arc<Self>,
        every: Duration,
        active: impl Fn() -> bool + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                if !active() {
                    continue;
                }
                if let Err(e) = self.reconcile().await {
                    tracing::warn!(error = %e, "fleet reconcile failed");
                }
            }
        })
    }
}
