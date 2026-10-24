//! Progress graphs: skill trees and timed unlockables as one thing — nodes with prerequisites,
//! currency costs, level requirements, ranks and an optional unlock timer that can be skipped
//! for currency. All time-dependent logic takes an explicit `now`.

use std::collections::{BTreeMap, HashMap, HashSet};

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct LevelRequirement {
    pub track: String,
    pub level: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Every listed node must be at rank ≥ 1.
    pub requires: Vec<String>,
    /// `{currency: amount}` paid for each rank.
    pub cost: Value,
    pub min_level: Option<LevelRequirement>,
    /// 0 = unlocks immediately, otherwise the rank is ready after this many seconds.
    pub unlock_seconds: i64,
    /// `{currency: amount}` per remaining hour to finish the timer now; empty = cannot skip.
    pub skip_cost_per_hour: Value,
    /// Reward table granted when a rank completes.
    pub rewards: Option<String>,
    pub max_rank: i32,
    /// Opaque payload for the game (stat bonuses, feature flags …).
    pub grants: Value,
}

impl Default for GraphNode {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            requires: Vec::new(),
            cost: json!({}),
            min_level: None,
            unlock_seconds: 0,
            skip_cost_per_hour: json!({}),
            rewards: None,
            max_rank: 1,
            grants: json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressGraph {
    pub id: String,
    pub name: String,
    pub description: String,
    pub nodes: Vec<GraphNode>,
    /// `{currency: amount}` to reset the whole graph.
    pub reset_cost: Value,
    /// Share (0–100) of the currency spent on nodes that a reset refunds.
    pub refund_pct: i32,
    pub enabled: bool,
}

impl Default for ProgressGraph {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            nodes: Vec::new(),
            reset_cost: json!({}),
            refund_pct: 0,
            enabled: true,
        }
    }
}

type GraphRow = (String, String, String, Value, Value, i32, bool);
const GRAPH_COLS: &str = "id, name, description, nodes, reset_cost, refund_pct, enabled";

fn graph(r: GraphRow) -> ProgressGraph {
    ProgressGraph {
        id: r.0,
        name: r.1,
        description: r.2,
        nodes: serde_json::from_value(r.3).unwrap_or_default(),
        reset_cost: r.4,
        refund_pct: r.5,
        enabled: r.6,
    }
}

/// `{currency: amount}` → validated list of positive integer amounts.
fn costs(v: &Value, what: &str) -> Result<Vec<(String, i64)>> {
    let Some(obj) = v.as_object() else {
        if v.is_null() {
            return Ok(Vec::new());
        }
        return Err(Error::InvalidArgument(format!(
            "{what} must be an object {{currency: amount}}"
        )));
    };
    let mut out = Vec::with_capacity(obj.len());
    for (c, a) in obj {
        let amount = a.as_i64().filter(|a| *a > 0).ok_or_else(|| {
            Error::InvalidArgument(format!("{what}.{c} must be a positive integer"))
        })?;
        crate::check_code(c)?;
        out.push((c.clone(), amount));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct NodeState {
    rank: i32,
    unlocks_at: Option<OffsetDateTime>,
    spent: BTreeMap<String, i64>,
}

type StateRow = (String, i32, Option<OffsetDateTime>, Value);

fn add_spent(spent: &mut BTreeMap<String, i64>, paid: &[(String, i64)]) {
    for (c, a) in paid {
        *spent.entry(c.clone()).or_default() += a;
    }
}

fn skip_cost(node: &GraphNode, remaining: Duration) -> Result<Vec<(String, i64)>> {
    let hours = remaining.as_seconds_f64().max(0.0) / 3600.0;
    Ok(costs(&node.skip_cost_per_hour, "skip_cost_per_hour")?
        .into_iter()
        .map(|(c, per_hour)| (c, ((per_hour as f64 * hours).ceil() as i64).max(1)))
        .collect())
}

fn ts(t: OffsetDateTime) -> Value {
    t.format(&Rfc3339).map(Value::String).unwrap_or(Value::Null)
}

impl EconomyService {
    pub async fn upsert_graph(&self, g: ProgressGraph) -> Result<ProgressGraph> {
        crate::check_code(&g.id)?;
        if !(0..=100).contains(&g.refund_pct) {
            return Err(Error::InvalidArgument("refund_pct must be 0..=100".into()));
        }
        costs(&g.reset_cost, "reset_cost")?;
        let ids: HashSet<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        if ids.len() != g.nodes.len() {
            return Err(Error::InvalidArgument("duplicate node id".into()));
        }
        for n in &g.nodes {
            crate::check_code(&n.id)?;
            if n.max_rank < 1 {
                return Err(Error::InvalidArgument(format!(
                    "node {}: max_rank must be ≥ 1",
                    n.id
                )));
            }
            if n.unlock_seconds < 0 {
                return Err(Error::InvalidArgument(format!(
                    "node {}: unlock_seconds must be ≥ 0",
                    n.id
                )));
            }
            costs(&n.cost, "cost")?;
            costs(&n.skip_cost_per_hour, "skip_cost_per_hour")?;
            if let Some(l) = &n.min_level {
                if l.track.is_empty() || l.level < 1 {
                    return Err(Error::InvalidArgument(format!(
                        "node {}: min_level needs a track and level ≥ 1",
                        n.id
                    )));
                }
            }
            for r in &n.requires {
                if r == &n.id || !ids.contains(r.as_str()) {
                    return Err(Error::InvalidArgument(format!(
                        "node {}: unknown prerequisite {r}",
                        n.id
                    )));
                }
            }
        }
        // prerequisites must form a DAG, otherwise part of the graph can never be reached
        let mut indeg: HashMap<&str, usize> = g
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.requires.len()))
            .collect();
        let mut ready: Vec<&str> = indeg
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut seen = 0;
        while let Some(id) = ready.pop() {
            seen += 1;
            for n in g
                .nodes
                .iter()
                .filter(|n| n.requires.iter().any(|r| r == id))
            {
                let d = indeg.get_mut(n.id.as_str()).unwrap();
                *d -= 1;
                if *d == 0 {
                    ready.push(n.id.as_str());
                }
            }
        }
        if seen != g.nodes.len() {
            return Err(Error::InvalidArgument(
                "prerequisites contain a cycle".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.progress_graphs (id, name, description, nodes, reset_cost, refund_pct, enabled)
             values ($1, $2, $3, $4, $5, $6, $7)
             on conflict (id) do update set name = excluded.name, description = excluded.description,
               nodes = excluded.nodes, reset_cost = excluded.reset_cost, refund_pct = excluded.refund_pct,
               enabled = excluded.enabled, updated_at = now()",
        )
        .bind(&g.id)
        .bind(&g.name)
        .bind(&g.description)
        .bind(serde_json::to_value(&g.nodes)?)
        .bind(&g.reset_cost)
        .bind(g.refund_pct)
        .bind(g.enabled)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(g)
    }

    pub async fn graphs(&self, include_disabled: bool) -> Result<Vec<ProgressGraph>> {
        let rows: Vec<GraphRow> = sqlx::query_as(&format!(
            "select {GRAPH_COLS} from musubi.progress_graphs where enabled or $1 order by id"
        ))
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(graph).collect())
    }

    async fn load_graph(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<ProgressGraph> {
        let row: Option<GraphRow> = sqlx::query_as(&format!(
            "select {GRAPH_COLS} from musubi.progress_graphs where id = $1"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let Some(g) = row.map(graph) else {
            return Err(Error::NotFound(format!("progress graph {id}")));
        };
        if !g.enabled {
            return Err(Error::PermissionDenied("progress graph disabled".into()));
        }
        Ok(g)
    }

    async fn load_states(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        graph_id: &str,
        lock: bool,
    ) -> Result<HashMap<String, NodeState>> {
        let rows: Vec<StateRow> = sqlx::query_as(&format!(
            "select node_id, rank, unlocks_at, spent from musubi.graph_progress
             where account_id = $1 and graph_id = $2{}",
            if lock { " for update" } else { "" }
        ))
        .bind(account.0)
        .bind(graph_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.0,
                    NodeState {
                        rank: r.1,
                        unlocks_at: r.2,
                        spent: serde_json::from_value(r.3).unwrap_or_default(),
                    },
                )
            })
            .collect())
    }

    async fn save_state(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        graph_id: &str,
        node_id: &str,
        s: &NodeState,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.graph_progress (account_id, graph_id, node_id, rank, unlocks_at, spent, updated_at)
             values ($1, $2, $3, $4, $5, $6, now())
             on conflict (account_id, graph_id, node_id) do update set rank = excluded.rank,
               unlocks_at = excluded.unlocks_at, spent = excluded.spent, updated_at = now()",
        )
        .bind(account.0)
        .bind(graph_id)
        .bind(node_id)
        .bind(s.rank)
        .bind(s.unlocks_at)
        .bind(serde_json::to_value(&s.spent)?)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    async fn pay(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        cost: &[(String, i64)],
        reason: &str,
        reference: &str,
    ) -> Result<()> {
        for (c, a) in cost {
            Self::adjust_tx(tx, account, c, -a, reason, reference, None, json!({})).await?;
        }
        Ok(())
    }

    /// Why a node cannot be started right now (prerequisites, level), if anything.
    async fn blocked_by(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        node: &GraphNode,
        states: &HashMap<String, NodeState>,
    ) -> Result<Option<String>> {
        for r in &node.requires {
            if states.get(r).map(|s| s.rank).unwrap_or(0) < 1 {
                return Ok(Some(format!("requires {r}")));
            }
        }
        if let Some(l) = &node.min_level {
            let lvl: Option<(i32,)> = sqlx::query_as(
                "select level from musubi.progress where account_id = $1 and track_id = $2",
            )
            .bind(account.0)
            .bind(&l.track)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            if lvl.map(|l| l.0).unwrap_or(0) < l.level {
                return Ok(Some(format!("requires level {} on {}", l.level, l.track)));
            }
        }
        Ok(None)
    }

    /// Completes the rank in progress (or an instant one): bumps rank, grants the node's table.
    async fn complete_rank(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        graph_id: &str,
        node: &GraphNode,
        state: &mut NodeState,
    ) -> Result<Vec<crate::rewards::RewardEntry>> {
        state.rank += 1;
        state.unlocks_at = None;
        Self::save_state(tx, account, graph_id, &node.id, state).await?;
        let mut rewards = Vec::new();
        if let Some(table_id) = node.rewards.as_deref() {
            let Some(table) = Self::load_table(tx, table_id).await? else {
                return Err(Error::NotFound(format!("reward table {table_id}")));
            };
            rewards = Self::apply_rewards_for(
                tx,
                account,
                Some(table_id),
                &table.entries,
                "graph.unlock",
                &format!("{graph_id}:{}:{}", node.id, state.rank),
            )
            .await?;
        }
        Ok(rewards)
    }

    fn node_view(
        node: &GraphNode,
        s: Option<&NodeState>,
        blocked: Option<&str>,
        now: OffsetDateTime,
    ) -> Value {
        let rank = s.map(|s| s.rank).unwrap_or(0);
        let unlocking = s.and_then(|s| s.unlocks_at);
        let state = match (unlocking, blocked) {
            (Some(t), _) if t > now => "unlocking",
            (Some(_), _) => "ready",
            _ if rank >= node.max_rank => "maxed",
            (_, Some(_)) => "locked",
            _ => "available",
        };
        let mut v = serde_json::to_value(node).unwrap_or_default();
        let o = v.as_object_mut().unwrap();
        o.insert("rank".into(), json!(rank));
        o.insert("state".into(), json!(state));
        if let Some(b) = blocked {
            o.insert("blocked_by".into(), json!(b));
        }
        if let Some(t) = unlocking {
            o.insert("unlocks_at".into(), ts(t));
            if t > now {
                if let Ok(c) = skip_cost(node, t - now) {
                    if !c.is_empty() {
                        o.insert(
                            "skip_cost".into(),
                            Value::Object(c.into_iter().map(|(k, a)| (k, json!(a))).collect()),
                        );
                    }
                }
            }
        }
        v
    }

    /// Graph definitions with the player's state on every node.
    pub async fn graph_views(
        &self,
        account: AccountId,
        graph_id: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Vec<Value>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let graphs = match graph_id {
            Some(id) => vec![Self::load_graph(&mut tx, id).await?],
            None => self.graphs(false).await?,
        };
        let mut out = Vec::with_capacity(graphs.len());
        for g in graphs {
            let states = Self::load_states(&mut tx, account, &g.id, false).await?;
            let mut nodes = Vec::with_capacity(g.nodes.len());
            for n in &g.nodes {
                let blocked = Self::blocked_by(&mut tx, account, n, &states).await?;
                nodes.push(Self::node_view(
                    n,
                    states.get(&n.id),
                    blocked.as_deref(),
                    now,
                ));
            }
            out.push(json!({
                "id": g.id, "name": g.name, "description": g.description,
                "reset_cost": g.reset_cost, "refund_pct": g.refund_pct, "nodes": nodes,
            }));
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(out)
    }

    /// Pays the node's cost and starts the next rank: instantly complete, or a timer.
    pub async fn graph_unlock(
        &self,
        account: AccountId,
        graph_id: &str,
        node_id: &str,
        now: OffsetDateTime,
    ) -> Result<Value> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        // The first unlock of a node has no state row to lock: serialise per (account, graph).
        Self::lock_key(&mut tx, "graph", &format!("{}:{graph_id}", account.0)).await?;
        let g = Self::load_graph(&mut tx, graph_id).await?;
        let Some(node) = g.nodes.iter().find(|n| n.id == node_id) else {
            return Err(Error::NotFound(format!("node {node_id}")));
        };
        let states = Self::load_states(&mut tx, account, graph_id, true).await?;
        let mut state = states.get(node_id).cloned().unwrap_or(NodeState {
            rank: 0,
            unlocks_at: None,
            spent: BTreeMap::new(),
        });
        if state.unlocks_at.is_some() {
            return Err(Error::Conflict("rank already unlocking".into()));
        }
        if state.rank >= node.max_rank {
            return Err(Error::Conflict("node at max rank".into()));
        }
        if let Some(b) = Self::blocked_by(&mut tx, account, node, &states).await? {
            return Err(Error::Conflict(b));
        }
        let cost = costs(&node.cost, "cost")?;
        let reference = format!("{graph_id}:{node_id}:{}", state.rank + 1);
        Self::pay(&mut tx, account, &cost, "graph.unlock", &reference).await?;
        add_spent(&mut state.spent, &cost);
        let mut rewards = Vec::new();
        if node.unlock_seconds > 0 {
            state.unlocks_at = Some(now + Duration::seconds(node.unlock_seconds));
            Self::save_state(&mut tx, account, graph_id, node_id, &state).await?;
        } else {
            rewards = Self::complete_rank(&mut tx, account, graph_id, node, &mut state).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({
            "node": Self::node_view(node, Some(&state), None, now),
            "paid": Value::Object(cost.into_iter().map(|(k, a)| (k, json!(a))).collect()),
            "rewards": rewards,
        }))
    }

    /// Finishes a timer that has run out.
    pub async fn graph_claim(
        &self,
        account: AccountId,
        graph_id: &str,
        node_id: &str,
        now: OffsetDateTime,
    ) -> Result<Value> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let g = Self::load_graph(&mut tx, graph_id).await?;
        let Some(node) = g.nodes.iter().find(|n| n.id == node_id) else {
            return Err(Error::NotFound(format!("node {node_id}")));
        };
        let states = Self::load_states(&mut tx, account, graph_id, true).await?;
        let Some(mut state) = states.get(node_id).cloned() else {
            return Err(Error::Conflict("nothing to claim".into()));
        };
        match state.unlocks_at {
            None => return Err(Error::Conflict("nothing to claim".into())),
            Some(t) if t > now => {
                return Err(Error::Conflict(format!(
                    "still unlocking, {} s left",
                    (t - now).whole_seconds()
                )))
            }
            Some(_) => {}
        }
        let rewards = Self::complete_rank(&mut tx, account, graph_id, node, &mut state).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({ "node": Self::node_view(node, Some(&state), None, now), "rewards": rewards }))
    }

    /// Pays `skip_cost_per_hour` × remaining time (rounded up) and completes the timer now.
    pub async fn graph_skip(
        &self,
        account: AccountId,
        graph_id: &str,
        node_id: &str,
        now: OffsetDateTime,
    ) -> Result<Value> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let g = Self::load_graph(&mut tx, graph_id).await?;
        let Some(node) = g.nodes.iter().find(|n| n.id == node_id) else {
            return Err(Error::NotFound(format!("node {node_id}")));
        };
        let states = Self::load_states(&mut tx, account, graph_id, true).await?;
        let Some(mut state) = states.get(node_id).cloned() else {
            return Err(Error::Conflict("nothing to skip".into()));
        };
        let Some(t) = state.unlocks_at else {
            return Err(Error::Conflict("nothing to skip".into()));
        };
        let cost = if t > now {
            let c = skip_cost(node, t - now)?;
            if c.is_empty() {
                return Err(Error::PermissionDenied(
                    "this node cannot be skipped".into(),
                ));
            }
            c
        } else {
            Vec::new()
        };
        let reference = format!("{graph_id}:{node_id}:{}:skip", state.rank + 1);
        Self::pay(&mut tx, account, &cost, "graph.skip", &reference).await?;
        add_spent(&mut state.spent, &cost);
        let rewards = Self::complete_rank(&mut tx, account, graph_id, node, &mut state).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({
            "node": Self::node_view(node, Some(&state), None, now),
            "paid": Value::Object(cost.into_iter().map(|(k, a)| (k, json!(a))).collect()),
            "rewards": rewards,
        }))
    }

    /// Pays `reset_cost`, refunds `refund_pct` of everything spent on nodes and forgets all ranks.
    pub async fn graph_reset(&self, account: AccountId, graph_id: &str) -> Result<Value> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let g = Self::load_graph(&mut tx, graph_id).await?;
        let states = Self::load_states(&mut tx, account, graph_id, true).await?;
        if states.is_empty() {
            return Err(Error::Conflict("nothing to reset".into()));
        }
        let cost = costs(&g.reset_cost, "reset_cost")?;
        Self::pay(&mut tx, account, &cost, "graph.reset", graph_id).await?;
        let mut refund: BTreeMap<String, i64> = BTreeMap::new();
        for s in states.values() {
            for (c, a) in &s.spent {
                let e = refund.entry(c.clone()).or_default();
                *e = e.saturating_add(a.saturating_mul(g.refund_pct as i64) / 100);
            }
        }
        for (c, a) in refund.iter().filter(|(_, a)| **a > 0) {
            Self::adjust_tx(
                &mut tx,
                account,
                c,
                *a,
                "graph.refund",
                graph_id,
                None,
                json!({}),
            )
            .await?;
        }
        sqlx::query("delete from musubi.graph_progress where account_id = $1 and graph_id = $2")
            .bind(account.0)
            .bind(graph_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({
            "paid": Value::Object(cost.into_iter().map(|(k, a)| (k, json!(a))).collect()),
            "refunded": refund,
        }))
    }
}
