//! Network egress accounting per game-server process (Hathora parity). The server reports its
//! cumulative socket counters (`POST /v1/gameserver/egress {bytes_out, bytes_in}`, e.g. every
//! 10–60 s); the API keeps totals, the rate between the last two reports, and rolls everything up
//! per fleet with `spec.cost.egress_per_gb`.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, GameServerAuth, ServerKey};
use crate::app::AppState;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/gameserver/egress", post(report))
        .route("/v1/server/gameservers/{id}/egress", get(server_egress))
        .route("/v1/server/fleets/{id}/egress", get(fleet_egress))
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad id".into()).into())
}

#[derive(Deserialize)]
struct Report {
    /// Cumulative bytes sent by the process since start.
    bytes_out: i64,
    #[serde(default)]
    bytes_in: i64,
}

type Row = (
    Uuid,
    Uuid,
    i64,
    i64,
    f64,
    f64,
    i32,
    time::OffsetDateTime,
    time::OffsetDateTime,
);
const COLS: &str =
    "server_id, fleet_id, bytes_out, bytes_in, rate_out, rate_in, reports, first_at, updated_at";

fn card(r: &Row) -> Value {
    json!({
        "server_id": r.0, "fleet_id": r.1,
        "bytes_out": r.2, "bytes_in": r.3, "gib_out": r.2 as f64 / GIB,
        "rate_out_bps": r.4, "rate_in_bps": r.5, "reports": r.6,
        "first_at": r.7.format(&time::format_description::well_known::Rfc3339).ok(),
        "updated_at": r.8.format(&time::format_description::well_known::Rfc3339).ok(),
    })
}

async fn report(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
    Json(b): Json<Report>,
) -> Result<Json<Value>, ApiError> {
    let id = sid;
    if b.bytes_out < 0 || b.bytes_in < 0 {
        return Err(Error::InvalidArgument("bytes_out / bytes_in must be ≥ 0".into()).into());
    }
    let fleet: Option<(Uuid,)> = sqlx::query_as(
        "select fleet_id from musubi.game_servers where id = $1 and state <> 'terminated'",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((fleet_id,)) = fleet else {
        return Err(Error::NotFound("game server".into()).into());
    };
    // counters are cumulative: a smaller value means the process restarted — keep the total
    // monotonic by adding the new counter from zero.
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.server_egress as e (server_id, fleet_id, bytes_out, bytes_in, reports)
         values ($1, $2, $3, $4, 1)
         on conflict (server_id) do update set
             rate_out = case when excluded.bytes_out >= e.bytes_out and extract(epoch from now() - e.updated_at) > 0
                             then (excluded.bytes_out - e.bytes_out) / extract(epoch from now() - e.updated_at) else e.rate_out end,
             rate_in  = case when excluded.bytes_in >= e.bytes_in and extract(epoch from now() - e.updated_at) > 0
                             then (excluded.bytes_in - e.bytes_in) / extract(epoch from now() - e.updated_at) else e.rate_in end,
             bytes_out = case when excluded.bytes_out >= e.bytes_out then excluded.bytes_out else e.bytes_out + excluded.bytes_out end,
             bytes_in  = case when excluded.bytes_in >= e.bytes_in then excluded.bytes_in else e.bytes_in + excluded.bytes_in end,
             reports = e.reports + 1, updated_at = now()
         returning {COLS}"
    ))
    .bind(id)
    .bind(fleet_id)
    .bind(b.bytes_out)
    .bind(b.bytes_in)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(card(&row)))
}

async fn server_egress(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.server_egress where server_id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match row {
        Some(r) => Ok(Json(card(&r))),
        None => {
            let known: Option<(Uuid,)> =
                sqlx::query_as("select fleet_id from musubi.game_servers where id = $1")
                    .bind(id)
                    .fetch_optional(&s.db)
                    .await
                    .map_err(Error::internal)?;
            let Some((fleet_id,)) = known else {
                return Err(Error::NotFound("game server".into()).into());
            };
            Ok(Json(
                json!({ "server_id": id, "fleet_id": fleet_id, "bytes_out": 0, "bytes_in": 0,
                "gib_out": 0.0, "rate_out_bps": 0.0, "rate_in_bps": 0.0, "reports": 0 }),
            ))
        }
    }
}

async fn fleet_egress(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let f = s.fleets.get(id).await?;
    let (out, inn, live_rate_out, live_rate_in, servers, live): (i64, i64, f64, f64, i64, i64) =
        sqlx::query_as(
            "select coalesce(sum(e.bytes_out), 0)::bigint, coalesce(sum(e.bytes_in), 0)::bigint,
                coalesce(sum(e.rate_out) filter (where g.state <> 'terminated'), 0)::float8,
                coalesce(sum(e.rate_in) filter (where g.state <> 'terminated'), 0)::float8,
                count(*)::bigint,
                count(*) filter (where g.state <> 'terminated')::bigint
         from musubi.server_egress e left join musubi.game_servers g on g.id = e.server_id
         where e.fleet_id = $1",
        )
        .bind(id)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    let top: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.server_egress where fleet_id = $1 order by bytes_out desc limit 10"
    ))
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let cost = f.spec.cost.as_ref();
    let per_gb = cost.map(|c| c.egress_per_gb).unwrap_or(0.0);
    let gib_out = out as f64 / GIB;
    Ok(Json(json!({
        "fleet_id": id,
        "bytes_out": out, "bytes_in": inn, "gib_out": gib_out, "gib_in": inn as f64 / GIB,
        "live_rate_out_bps": live_rate_out, "live_rate_in_bps": live_rate_in,
        "servers_reporting": servers, "live_servers_reporting": live,
        "cost": {
            "configured": per_gb > 0.0,
            "egress_per_gb": per_gb,
            "currency": cost.map(|c| c.currency.clone()).unwrap_or_default(),
            "total": gib_out * per_gb,
            "projected_daily": live_rate_out * 86_400.0 / GIB * per_gb,
        },
        "top": top.iter().map(card).collect::<Vec<_>>(),
    })))
}
