//! Asynchronous PvP matchmaking (P1-55): opponents from the rating ladder, attacks and results,
//! shields, opt-out, revenge. Rules per rating queue are set by the game server.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_leaderboard::MatchResult;
use musubi_social::NewNotification;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::async_mm::{AsyncMatchService, Rules};
use crate::http::{ApiError, Bearer, ServerKey};

const ATTACK_CODE: i32 = 33;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/matchmaker/async", get(all_rules))
        .route(
            "/v1/server/matchmaker/async/{queue}",
            get(get_rules).put(put_rules).delete(delete_rules),
        )
        .route(
            "/v1/server/matchmaker/async/{queue}/shields",
            post(server_shield),
        )
        .route(
            "/v1/server/matchmaker/async/{queue}/shields/{account_id}",
            delete(server_drop_shield),
        )
        .route(
            "/v1/server/matchmaker/async/{queue}/attacks",
            get(server_attacks),
        )
        .route(
            "/v1/server/matchmaker/async/attacks/{id}/resolve",
            post(server_resolve),
        )
        .route("/v1/matchmaker/async/{queue}/opponents", get(opponents))
        .route("/v1/matchmaker/async/{queue}/attacks", post(attack))
        .route(
            "/v1/matchmaker/async/{queue}/attacks/{id}/resolve",
            post(resolve),
        )
        .route("/v1/matchmaker/async/{queue}/me", get(me))
        .route("/v1/matchmaker/async/{queue}/opt_out", put(opt_out))
        .route("/v1/matchmaker/async/{queue}/shield", post(buy_shield))
}

async fn all_rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let all = s.async_mm.all_rules().await?;
    Ok(Json(json!({
        "queues": all.into_iter().map(|(q, r)| json!({ "queue": q, "rules": r })).collect::<Vec<_>>()
    })))
}

async fn get_rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(queue): Path<String>,
) -> Result<Json<Rules>, ApiError> {
    Ok(Json(s.async_mm.rules(&queue).await?))
}

/// The rating queue must exist: results of attacks are applied to it.
async fn put_rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(queue): Path<String>,
    Json(rules): Json<Rules>,
) -> Result<Json<Rules>, ApiError> {
    s.ratings.queue(&queue).await?;
    Ok(Json(s.async_mm.put_rules(&queue, rules).await?))
}

async fn delete_rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(queue): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "deleted": s.async_mm.delete_rules(&queue).await? }),
    ))
}

#[derive(Deserialize)]
struct ShieldReq {
    account_id: AccountId,
    secs: i64,
    #[serde(default)]
    source: Option<String>,
}

async fn server_shield(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(queue): Path<String>,
    Json(r): Json<ShieldReq>,
) -> Result<Json<Value>, ApiError> {
    let rules = s.async_mm.rules(&queue).await?;
    let sh = s
        .async_mm
        .grant_shield(
            &rules,
            &queue,
            r.account_id,
            r.secs,
            r.source.as_deref().unwrap_or("server"),
        )
        .await?;
    Ok(Json(json!(sh)))
}

async fn server_drop_shield(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((queue, account)): Path<(String, AccountId)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "dropped": s.async_mm.drop_shield(&queue, account).await? }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    account_id: Option<AccountId>,
    /// attacker | defender | any (default any)
    role: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
}

async fn server_attacks(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(queue): Path<String>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    s.async_mm.expire(&queue).await?;
    let list = s
        .async_mm
        .list(
            &queue,
            q.account_id,
            q.role.as_deref().unwrap_or("any"),
            q.state.as_deref(),
            q.limit.unwrap_or(50),
        )
        .await?;
    Ok(Json(json!({ "attacks": list })))
}

#[derive(Deserialize)]
struct ResolveReq {
    result: String,
    #[serde(default)]
    data: Option<Value>,
}

async fn server_resolve(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Json(r): Json<ResolveReq>,
) -> Result<Json<Value>, ApiError> {
    finish(&s, id, None, r).await
}

/// Apply a result: rating update for both sides, loser's shield, defender's notification.
async fn finish(
    s: &AppState,
    id: Uuid,
    by: Option<AccountId>,
    r: ResolveReq,
) -> Result<Json<Value>, ApiError> {
    let a = s.async_mm.attack(id).await?;
    if by.is_some_and(|me| me != a.attacker) {
        return Err(Error::PermissionDenied("not your attack".into()).into());
    }
    let rules = s.async_mm.rules(&a.queue_id).await?;
    let a = s.async_mm.resolve(id, &r.result, r.data).await?;
    let applied = s
        .ratings
        .apply(
            &a.queue_id,
            MatchResult {
                teams: vec![vec![a.attacker], vec![a.defender]],
                ranks: Some(AsyncMatchService::ranks(&r.result)),
                match_id: Some(a.id.to_string()),
            },
        )
        .await?;
    let mut shield = None;
    if r.result == "win" && rules.shield_on_loss_secs > 0 {
        shield = Some(
            s.async_mm
                .grant_shield(
                    &rules,
                    &a.queue_id,
                    a.defender,
                    rules.shield_on_loss_secs,
                    "loss",
                )
                .await?,
        );
    }
    let defender_result = match r.result.as_str() {
        "win" => "loss",
        "loss" => "win",
        _ => "draw",
    };
    let _ = s
        .notify
        .send(vec![NewNotification {
            user_id: a.defender,
            sender_id: Some(a.attacker),
            subject: "async_attack".into(),
            content: json!({ "attack_id": a.id, "queue": a.queue_id, "attacker": a.attacker,
                             "result": defender_result, "revenge_until_secs": rules.revenge_ttl_secs,
                             "shield": shield }),
            code: ATTACK_CODE,
            persistent: true,
            ttl_secs: Some(7 * 86_400),
        }])
        .await;
    s.events
        .emit(
            "async_mm.attack_resolved",
            Some(a.attacker),
            json!({ "attack": AsyncMatchService::summary(&a), "rating": applied.id }),
        )
        .await;
    Ok(Json(
        json!({ "attack": a, "rating": applied, "defender_shield": shield }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OpponentsQ {
    count: Option<usize>,
    spread: Option<f64>,
}

/// Attackable players near my rating: within `spread` first, widened to `max_spread` when too
/// few; each with their public rating and whether this would be revenge.
async fn opponents(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
    Query(q): Query<OpponentsQ>,
) -> Result<Json<Value>, ApiError> {
    let rules = s.async_mm.rules(&queue).await?;
    if !rules.enabled {
        return Err(Error::PermissionDenied("async matchmaking is disabled".into()).into());
    }
    let rq = s.ratings.queue(&queue).await?;
    s.async_mm.expire(&queue).await?;
    let count = q.count.unwrap_or(rules.count).clamp(1, rules.max_count);
    let spread = q
        .spread
        .unwrap_or(rules.spread)
        .clamp(1.0, rules.max_spread);
    let mine = s.ratings.get(&queue, me.account_id).await.ok();
    let (my_mu, my_display) = mine
        .as_ref()
        .map(|r| (Some(r.mu), r.display))
        .unwrap_or((None, 1500.0));
    let ids = s
        .async_mm
        .nearest(
            &rules,
            &queue,
            rq.season,
            me.account_id,
            my_mu,
            (count * 20) as i64,
        )
        .await?;
    let display = if ids.is_empty() {
        Default::default()
    } else {
        s.ratings.display_many(&queue, &ids).await?
    };
    let mut ranked: Vec<(AccountId, f64)> = ids
        .iter()
        .filter_map(|id| display.get(id).map(|d| (*id, *d)))
        .collect();
    ranked.sort_by(|a, b| {
        (a.1 - my_display)
            .abs()
            .total_cmp(&(b.1 - my_display).abs())
    });
    let within = |max: f64| {
        ranked
            .iter()
            .filter(|(_, d)| (d - my_display).abs() <= max)
            .take(count)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    let mut picked = within(spread);
    let mut used_spread = spread;
    if picked.len() < count && rules.max_spread > spread {
        picked = within(rules.max_spread);
        used_spread = rules.max_spread;
    }
    let incoming = s
        .async_mm
        .list(
            &queue,
            Some(me.account_id),
            "defender",
            Some("resolved"),
            100,
        )
        .await?;
    let mut out = Vec::new();
    for id in picked {
        let r = s.ratings.get(&queue, id).await?;
        let mut revenge_of = None;
        for a in incoming.iter().filter(|a| a.attacker == r.account_id) {
            if s.async_mm.revenge_open(&rules, me.account_id, a).await? {
                revenge_of = Some(a.id);
                break;
            }
        }
        out.push(json!({
            "account_id": r.account_id, "rating": r.display, "games": r.games,
            "wins": r.wins, "losses": r.losses, "last_played": r.last_played,
            "distance": (r.display - my_display).abs(), "revenge_of": revenge_of,
        }));
    }
    Ok(Json(json!({
        "opponents": out, "my_rating": my_display, "spread": used_spread, "count": count,
    })))
}

#[derive(Deserialize)]
struct AttackReq {
    defender_id: AccountId,
    #[serde(default)]
    revenge_of: Option<Uuid>,
    #[serde(default)]
    data: Value,
}

async fn attack(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
    Json(r): Json<AttackReq>,
) -> Result<Json<Value>, ApiError> {
    let rules = s.async_mm.rules(&queue).await?;
    s.ratings.queue(&queue).await?;
    let a = s
        .async_mm
        .start(
            &rules,
            &queue,
            me.account_id,
            r.defender_id,
            r.revenge_of,
            r.data,
        )
        .await?;
    s.events
        .emit(
            "async_mm.attack_started",
            Some(me.account_id),
            AsyncMatchService::summary(&a),
        )
        .await;
    Ok(Json(json!(a)))
}

async fn resolve(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((_queue, id)): Path<(String, Uuid)>,
    Json(r): Json<ResolveReq>,
) -> Result<Json<Value>, ApiError> {
    finish(&s, id, Some(me.account_id), r).await
}

/// My side of the queue: shield, opt-out, open attacks, recent incoming with revenge flags,
/// daily budget.
async fn me(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rules = s.async_mm.rules(&queue).await?;
    s.async_mm.expire(&queue).await?;
    let shield = s.async_mm.shield(&queue, me.account_id).await?;
    let opted_out = s.async_mm.opted_out(&queue, me.account_id).await?;
    let open = s.async_mm.open_by(&queue, me.account_id).await?;
    let incoming = s
        .async_mm
        .list(
            &queue,
            Some(me.account_id),
            "defender",
            Some("resolved"),
            20,
        )
        .await?;
    let mut inc = Vec::new();
    for a in incoming {
        let revenge = s.async_mm.revenge_open(&rules, me.account_id, &a).await?;
        let mut v = serde_json::to_value(&a).map_err(Error::internal)?;
        v["revenge_available"] = json!(revenge);
        inc.push(v);
    }
    let today = s.async_mm.attacks_today(&queue, me.account_id).await?;
    Ok(Json(json!({
        "shield": shield, "opted_out": opted_out, "open": open, "incoming": inc,
        "attacks_today": today,
        "attacks_left_today": (rules.attacks_per_day > 0).then(|| (rules.attacks_per_day - today).max(0)),
        "shield_price": rules.shield_price,
    })))
}

#[derive(Deserialize)]
struct OptOutReq {
    opted_out: bool,
}

async fn opt_out(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
    Json(r): Json<OptOutReq>,
) -> Result<Json<Value>, ApiError> {
    s.async_mm.rules(&queue).await?;
    s.async_mm
        .set_opt_out(&queue, me.account_id, r.opted_out)
        .await?;
    Ok(Json(json!({ "opted_out": r.opted_out })))
}

#[derive(Deserialize)]
struct BuyShieldReq {
    hours: i64,
}

/// `(shield seconds, wallet cost)` for buying `hours` of shield. All arithmetic is checked: an
/// unchecked `hours * 3_600` / `per_hour * hours` wraps in release builds, which would turn a
/// huge `hours` into a negative "cost" and credit the wallet instead of debiting it.
fn shield_purchase(
    rules: &Rules,
    price: &crate::async_mm::ShieldPrice,
    hours: i64,
) -> Result<(i64, i64), Error> {
    let max_hours = rules.shield_max_secs / 3_600;
    let bad = || Error::InvalidArgument(format!("hours must be 1..={max_hours}"));
    if hours <= 0 || hours > max_hours {
        return Err(bad());
    }
    let secs = hours.checked_mul(3_600).ok_or_else(bad)?;
    let cost = price
        .per_hour
        .checked_mul(hours)
        .filter(|c| *c > 0)
        .ok_or_else(|| Error::InvalidArgument("shield price overflows".into()))?;
    Ok((secs, cost))
}

/// Buy shield time with the queue's `shield_price` currency (wallet debit, ledger reason
/// `async_shield`).
async fn buy_shield(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
    Json(r): Json<BuyShieldReq>,
) -> Result<Json<Value>, ApiError> {
    let rules = s.async_mm.rules(&queue).await?;
    let price = rules
        .shield_price
        .clone()
        .ok_or_else(|| Error::PermissionDenied("shields are not for sale in this queue".into()))?;
    let (secs, cost) = shield_purchase(&rules, &price, r.hours)?;
    if !s.async_mm.open_by(&queue, me.account_id).await?.is_empty() {
        return Err(Error::Conflict("resolve your open attack first".into()).into());
    }
    let (entry, _) = s
        .economy
        .adjust(
            me.account_id,
            &price.currency,
            -cost,
            "async_shield",
            &queue,
            None,
            json!({ "hours": r.hours }),
        )
        .await?;
    let sh = s
        .async_mm
        .grant_shield(&rules, &queue, me.account_id, secs, "bought")
        .await?;
    Ok(Json(
        json!({ "shield": sh, "cost": cost, "currency": price.currency, "ledger": entry }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_mm::ShieldPrice;

    fn price(per_hour: i64) -> ShieldPrice {
        ShieldPrice {
            currency: "gold".into(),
            per_hour,
        }
    }

    #[test]
    fn shield_purchase_is_checked() {
        let rules = Rules {
            shield_max_secs: 7 * 86_400,
            ..Rules::default()
        };
        assert_eq!(shield_purchase(&rules, &price(10), 2).unwrap(), (7_200, 20));
        assert_eq!(
            shield_purchase(&rules, &price(10), 168).unwrap(),
            (604_800, 1_680)
        );
        assert!(shield_purchase(&rules, &price(10), 0).is_err());
        assert!(shield_purchase(&rules, &price(10), -5).is_err());
        assert!(shield_purchase(&rules, &price(10), 169).is_err());
        // values that used to wrap into a negative cost (= a wallet credit)
        assert!(shield_purchase(&rules, &price(10), i64::MAX / 3_600 + 1).is_err());
        assert!(shield_purchase(&rules, &price(10), i64::MAX).is_err());
        let huge = Rules {
            shield_max_secs: i64::MAX,
            ..Rules::default()
        };
        assert!(shield_purchase(&huge, &price(i64::MAX), 2).is_err());
        assert!(shield_purchase(&huge, &price(10), i64::MAX / 3_600).is_ok());
    }
}
