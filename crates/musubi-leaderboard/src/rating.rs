//! Skill rating per queue/mode (plan 5.6): Elo, Glicko-2 and OpenSkill (Weng-Lin, Bradley-Terry
//! full pairing), with seasons (soft reset), inactivity decay and placement games.
//!
//! One rating queue usually mirrors one matchmaker queue: the matchmaker fills a ticket's `mmr`
//! from the player's rating here when the client does not send one, so the MMR windows of the
//! queue work on the server's view of skill, not the client's.
//!
//! Storage: `mu`/`sigma`/`vol` columns hold the native numbers of the system (Elo: rating; Glicko-2:
//! rating, RD, volatility on the 1500 scale; OpenSkill: μ, σ). `display` maps all three onto one
//! 1500-centred scale so a queue can switch systems without retuning the matchmaker.

use std::collections::HashMap;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RatingSystem {
    #[default]
    Elo,
    Glicko2,
    Openskill,
}

impl RatingSystem {
    fn from_i16(v: i16) -> Self {
        match v {
            1 => Self::Glicko2,
            2 => Self::Openskill,
            _ => Self::Elo,
        }
    }
    fn as_i16(self) -> i16 {
        match self {
            Self::Elo => 0,
            Self::Glicko2 => 1,
            Self::Openskill => 2,
        }
    }
}

/// Tunables of a rating queue; every field has a sensible default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RatingParams {
    /// Elo K-factor.
    pub elo_k: f64,
    /// Games before a rating counts as placed; Elo uses `elo_k * placement_k_mult` until then.
    pub placement_games: u32,
    pub placement_k_mult: f64,
    /// Glicko-2 system constant (0.3..1.2; smaller = volatility moves less).
    pub glicko_tau: f64,
    /// OpenSkill β (performance noise); default σ₀/2.
    pub openskill_beta: Option<f64>,
    /// Inactivity before decay starts and the period of each decay step.
    pub decay_after_secs: u64,
    pub decay_every_secs: u64,
    /// Elo: points removed per step, never below `decay_floor` (default: the initial rating).
    /// Glicko-2 / OpenSkill decay widens uncertainty instead (RD / σ grow back towards initial).
    pub decay_points: f64,
    pub decay_floor: Option<f64>,
    /// Season soft reset: new = initial + carry × (old − initial).
    pub season_carry: f64,
}

impl Default for RatingParams {
    fn default() -> Self {
        Self {
            elo_k: 32.0,
            placement_games: 10,
            placement_k_mult: 2.0,
            glicko_tau: 0.5,
            openskill_beta: None,
            decay_after_secs: 14 * 86_400,
            decay_every_secs: 7 * 86_400,
            decay_points: 25.0,
            decay_floor: None,
            season_carry: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatingQueue {
    pub id: String,
    pub system: RatingSystem,
    pub params: RatingParams,
    pub season: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpsertRatingQueue {
    pub id: String,
    #[serde(default)]
    pub system: RatingSystem,
    #[serde(default)]
    pub params: RatingParams,
}

/// A player's rating in one queue and season.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rating {
    pub account_id: AccountId,
    pub queue_id: String,
    pub season: i32,
    pub system: RatingSystem,
    /// Native numbers of the system (see module docs).
    pub mu: f64,
    pub sigma: f64,
    pub vol: f64,
    /// 1500-centred display rating (what the matchmaker uses as `mmr`).
    pub display: f64,
    /// Conservative estimate (display minus uncertainty): rank by this for ladders.
    pub conservative: f64,
    pub games: i32,
    pub wins: i32,
    pub losses: i32,
    pub draws: i32,
    pub placed: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_played: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatchResult {
    /// Teams of account ids; a solo game is one player per team.
    pub teams: Vec<Vec<AccountId>>,
    /// One rank per team, 1 = best, equal ranks = draw. Default: team order is the ranking.
    #[serde(default)]
    pub ranks: Option<Vec<u32>>,
    #[serde(default)]
    pub match_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RatingChange {
    pub account_id: AccountId,
    pub team: u32,
    pub before: f64,
    pub after: f64,
    pub rating: Rating,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultApplied {
    pub id: Uuid,
    pub queue_id: String,
    pub season: i32,
    pub changes: Vec<RatingChange>,
}

// ---------------------------------------------------------------- pure math

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Skill {
    pub mu: f64,
    pub sigma: f64,
    pub vol: f64,
}

pub const ELO_INITIAL: f64 = 1500.0;
pub const GLICKO_INITIAL_RD: f64 = 350.0;
pub const GLICKO_INITIAL_VOL: f64 = 0.06;
pub const GLICKO_SCALE: f64 = 173.7178;
pub const OPENSKILL_MU: f64 = 25.0;
pub const OPENSKILL_SIGMA: f64 = 25.0 / 3.0;
const OPENSKILL_KAPPA: f64 = 0.0001;
/// Bounds for one `apply` call (OpenSkill is O(teams² · players)).
pub const MAX_RESULT_TEAMS: usize = 64;
pub const MAX_RESULT_PLAYERS: usize = 256;
/// OpenSkill μ → display: 25 ↦ 1500, one σ₀ (8.33) ↦ 500 points.
const OPENSKILL_DISPLAY_PER_MU: f64 = 60.0;

impl RatingSystem {
    pub fn initial(self) -> Skill {
        match self {
            Self::Elo => Skill {
                mu: ELO_INITIAL,
                sigma: 0.0,
                vol: 0.0,
            },
            Self::Glicko2 => Skill {
                mu: ELO_INITIAL,
                sigma: GLICKO_INITIAL_RD,
                vol: GLICKO_INITIAL_VOL,
            },
            Self::Openskill => Skill {
                mu: OPENSKILL_MU,
                sigma: OPENSKILL_SIGMA,
                vol: 0.0,
            },
        }
    }

    pub fn display(self, s: &Skill) -> f64 {
        match self {
            Self::Elo | Self::Glicko2 => s.mu,
            Self::Openskill => ELO_INITIAL + (s.mu - OPENSKILL_MU) * OPENSKILL_DISPLAY_PER_MU,
        }
    }

    pub fn conservative(self, s: &Skill) -> f64 {
        match self {
            Self::Elo => s.mu,
            Self::Glicko2 => s.mu - 2.0 * s.sigma,
            Self::Openskill => {
                ELO_INITIAL + (s.mu - 3.0 * s.sigma - OPENSKILL_MU) * OPENSKILL_DISPLAY_PER_MU
            }
        }
    }
}

/// Outcome of team `a` against team `b` from their ranks (1 = best): 1 win, 0.5 draw, 0 loss.
fn score(rank_a: u32, rank_b: u32) -> f64 {
    match rank_a.cmp(&rank_b) {
        std::cmp::Ordering::Less => 1.0,
        std::cmp::Ordering::Equal => 0.5,
        std::cmp::Ordering::Greater => 0.0,
    }
}

/// Elo over any number of teams: a team's rating is the mean of its members; every pair of teams
/// plays one virtual game, the mean of the pairwise deltas is applied to each member.
/// `k` per team member (placement K-factor can differ per player).
pub fn elo(teams: &[Vec<Skill>], ranks: &[u32], k: &[Vec<f64>]) -> Vec<Vec<Skill>> {
    let means: Vec<f64> = teams
        .iter()
        .map(|t| t.iter().map(|s| s.mu).sum::<f64>() / t.len().max(1) as f64)
        .collect();
    let n = teams.len();
    teams
        .iter()
        .enumerate()
        .map(|(i, team)| {
            let mut sum = 0.0;
            for j in 0..n {
                if i == j {
                    continue;
                }
                let expected = 1.0 / (1.0 + 10f64.powf((means[j] - means[i]) / 400.0));
                sum += score(ranks[i], ranks[j]) - expected;
            }
            let per_game = if n > 1 { sum / (n - 1) as f64 } else { 0.0 };
            team.iter()
                .enumerate()
                .map(|(m, s)| Skill {
                    mu: s.mu + k[i][m] * per_game,
                    ..*s
                })
                .collect()
        })
        .collect()
}

/// Glicko-2 (Glickman 2013), one rating period = this match. Each player meets every opposing
/// team as one composite opponent (mean rating, RMS deviation). Volatility by the Illinois
/// algorithm with convergence 1e-6.
pub fn glicko2(teams: &[Vec<Skill>], ranks: &[u32], tau: f64) -> Vec<Vec<Skill>> {
    let composite: Vec<(f64, f64)> = teams
        .iter()
        .map(|t| {
            let n = t.len().max(1) as f64;
            let mu = t
                .iter()
                .map(|s| (s.mu - ELO_INITIAL) / GLICKO_SCALE)
                .sum::<f64>()
                / n;
            let phi = (t
                .iter()
                .map(|s| (s.sigma / GLICKO_SCALE).powi(2))
                .sum::<f64>()
                / n)
                .sqrt();
            (mu, phi)
        })
        .collect();
    let g = |phi: f64| 1.0 / (1.0 + 3.0 * phi * phi / (std::f64::consts::PI.powi(2))).sqrt();
    let e = |mu: f64, mu_j: f64, phi_j: f64| 1.0 / (1.0 + (-g(phi_j) * (mu - mu_j)).exp());
    teams
        .iter()
        .enumerate()
        .map(|(i, team)| {
            team.iter()
                .map(|s| {
                    let mu = (s.mu - ELO_INITIAL) / GLICKO_SCALE;
                    let phi = s.sigma / GLICKO_SCALE;
                    let mut v_inv = 0.0;
                    let mut delta_sum = 0.0;
                    for (j, &(mu_j, phi_j)) in composite.iter().enumerate() {
                        if j == i {
                            continue;
                        }
                        let gj = g(phi_j);
                        let ej = e(mu, mu_j, phi_j);
                        v_inv += gj * gj * ej * (1.0 - ej);
                        delta_sum += gj * (score(ranks[i], ranks[j]) - ej);
                    }
                    if v_inv == 0.0 {
                        return *s;
                    }
                    let v = 1.0 / v_inv;
                    let delta = v * delta_sum;
                    let sigma = volatility(s.vol, delta, phi, v, tau);
                    let phi_star = (phi * phi + sigma * sigma).sqrt();
                    let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
                    let mu_new = mu + phi_new * phi_new * delta_sum;
                    Skill {
                        mu: ELO_INITIAL + mu_new * GLICKO_SCALE,
                        sigma: phi_new * GLICKO_SCALE,
                        vol: sigma,
                    }
                })
                .collect()
        })
        .collect()
}

fn volatility(sigma: f64, delta: f64, phi: f64, v: f64, tau: f64) -> f64 {
    let a = (sigma * sigma).ln();
    let f = |x: f64| {
        let ex = x.exp();
        let d2 = delta * delta;
        let p2 = phi * phi;
        ex * (d2 - p2 - v - ex) / (2.0 * (p2 + v + ex).powi(2)) - (x - a) / (tau * tau)
    };
    let eps = 1e-6;
    let mut big_a = a;
    let mut big_b = if delta * delta > phi * phi + v {
        (delta * delta - phi * phi - v).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * tau) < 0.0 {
            k += 1.0;
        }
        a - k * tau
    };
    let mut fa = f(big_a);
    let mut fb = f(big_b);
    let mut guard = 0;
    while (big_b - big_a).abs() > eps && guard < 100 {
        let c = big_a + (big_a - big_b) * fa / (fb - fa);
        let fc = f(c);
        if fc * fb <= 0.0 {
            big_a = big_b;
            fa = fb;
        } else {
            fa /= 2.0;
        }
        big_b = c;
        fb = fc;
        guard += 1;
    }
    (big_a / 2.0).exp()
}

/// OpenSkill / Weng-Lin Bradley-Terry with full pairing: every team against every other team,
/// a player's share of the team update is proportional to their σ².
pub fn openskill(teams: &[Vec<Skill>], ranks: &[u32], beta: f64) -> Vec<Vec<Skill>> {
    let agg: Vec<(f64, f64)> = teams
        .iter()
        .map(|t| {
            (
                t.iter().map(|s| s.mu).sum::<f64>(),
                t.iter().map(|s| s.sigma * s.sigma).sum::<f64>(),
            )
        })
        .collect();
    let n = teams.len();
    teams
        .iter()
        .enumerate()
        .map(|(i, team)| {
            let (mu_i, var_i) = agg[i];
            let mut omega = 0.0;
            let mut delta = 0.0;
            for (q, &(mu_q, var_q)) in agg.iter().enumerate() {
                if q == i {
                    continue;
                }
                let c = (var_i + var_q + 2.0 * beta * beta).sqrt();
                let p = (mu_i / c).exp() / ((mu_i / c).exp() + (mu_q / c).exp());
                let s = score(ranks[i], ranks[q]);
                omega += var_i / c * (s - p);
                let gamma = var_i.sqrt() / c;
                delta += gamma * var_i / (c * c) * p * (1.0 - p);
            }
            let _ = n;
            team.iter()
                .map(|s| {
                    let var = s.sigma * s.sigma;
                    let mu = s.mu + var / var_i * omega;
                    let sigma = (var * (1.0 - var / var_i * delta).max(OPENSKILL_KAPPA)).sqrt();
                    Skill {
                        mu,
                        sigma,
                        vol: 0.0,
                    }
                })
                .collect()
        })
        .collect()
}

/// One decay step for an inactive player.
pub fn decay_step(system: RatingSystem, p: &RatingParams, s: &Skill) -> Skill {
    let init = system.initial();
    match system {
        RatingSystem::Elo => Skill {
            mu: (s.mu - p.decay_points).max(p.decay_floor.unwrap_or(init.mu).min(s.mu)),
            ..*s
        },
        // Glicko-2 step 6 with no games: RD grows by the volatility, capped at the initial RD
        RatingSystem::Glicko2 => {
            let phi = s.sigma / GLICKO_SCALE;
            let phi_star = (phi * phi + s.vol * s.vol).sqrt() * GLICKO_SCALE;
            Skill {
                sigma: phi_star.min(init.sigma),
                ..*s
            }
        }
        RatingSystem::Openskill => Skill {
            sigma: (s.sigma * s.sigma + (init.sigma / 10.0).powi(2))
                .sqrt()
                .min(init.sigma),
            ..*s
        },
    }
}

/// Season rollover: rating moves `carry` of the way back from the initial value, uncertainty
/// widens so early games of the season move the rating faster.
pub fn soft_reset(system: RatingSystem, carry: f64, s: &Skill) -> Skill {
    let init = system.initial();
    let mu = init.mu + carry * (s.mu - init.mu);
    match system {
        RatingSystem::Elo => Skill { mu, ..*s },
        RatingSystem::Glicko2 => Skill {
            mu,
            sigma: s.sigma.max(init.sigma * 0.6),
            vol: s.vol,
        },
        RatingSystem::Openskill => Skill {
            mu,
            sigma: s.sigma.max(init.sigma * 0.6),
            vol: 0.0,
        },
    }
}

// ---------------------------------------------------------------- service

#[derive(Clone)]
pub struct RatingService {
    db: PgPool,
}

type QueueRow = (
    String,
    i16,
    serde_json::Value,
    i32,
    OffsetDateTime,
    OffsetDateTime,
);
type RatingRow = (
    Uuid,
    f64,
    f64,
    f64,
    i32,
    i32,
    i32,
    i32,
    Option<OffsetDateTime>,
);
const RATING_COLS: &str = "account_id, mu, sigma, vol, games, wins, losses, draws, last_played";

fn to_queue(r: QueueRow) -> Result<RatingQueue> {
    Ok(RatingQueue {
        id: r.0,
        system: RatingSystem::from_i16(r.1),
        params: serde_json::from_value(r.2).map_err(Error::internal)?,
        season: r.3,
        created_at: r.4,
        updated_at: r.5,
    })
}

fn to_rating(q: &RatingQueue, r: RatingRow) -> Rating {
    let s = Skill {
        mu: r.1,
        sigma: r.2,
        vol: r.3,
    };
    Rating {
        account_id: AccountId(r.0),
        queue_id: q.id.clone(),
        season: q.season,
        system: q.system,
        mu: s.mu,
        sigma: s.sigma,
        vol: s.vol,
        display: q.system.display(&s),
        conservative: q.system.conservative(&s),
        games: r.4,
        wins: r.5,
        losses: r.6,
        draws: r.7,
        placed: r.4 as u32 >= q.params.placement_games,
        last_played: r.8,
    }
}

fn fresh(q: &RatingQueue, account: AccountId) -> Rating {
    let s = q.system.initial();
    Rating {
        account_id: account,
        queue_id: q.id.clone(),
        season: q.season,
        system: q.system,
        mu: s.mu,
        sigma: s.sigma,
        vol: s.vol,
        display: q.system.display(&s),
        conservative: q.system.conservative(&s),
        games: 0,
        wins: 0,
        losses: 0,
        draws: 0,
        placed: q.params.placement_games == 0,
        last_played: None,
    }
}

impl RatingService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn upsert_queue(&self, c: UpsertRatingQueue) -> Result<RatingQueue> {
        crate::validate_id(&c.id)?;
        let p = &c.params;
        if !(0.05..=2.0).contains(&p.glicko_tau) || !(p.elo_k.is_finite() && p.elo_k > 0.0) {
            return Err(Error::InvalidArgument(
                "glicko_tau must be in 0.05..=2, elo_k > 0".into(),
            ));
        }
        let finite_positive = |v: f64| v.is_finite() && v > 0.0;
        if !finite_positive(p.placement_k_mult)
            || !p.openskill_beta.is_none_or(finite_positive)
            || !p.decay_points.is_finite()
            || !p.decay_floor.is_none_or(f64::is_finite)
        {
            return Err(Error::InvalidArgument(
                "placement_k_mult and openskill_beta must be finite and > 0, decay_points and decay_floor finite".into(),
            ));
        }
        if !(0.0..=1.0).contains(&c.params.season_carry) {
            return Err(Error::InvalidArgument(
                "season_carry must be in 0..=1".into(),
            ));
        }
        let params = serde_json::to_value(&c.params).map_err(Error::internal)?;
        let r = sqlx::query_as::<_, QueueRow>(
            "insert into musubi.rating_queues (id, system, params) values ($1, $2, $3)
             on conflict (id) do update set system = excluded.system, params = excluded.params,
               updated_at = now()
             returning id, system, params, season, created_at, updated_at",
        )
        .bind(&c.id)
        .bind(c.system.as_i16())
        .bind(params)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        to_queue(r)
    }

    pub async fn queue(&self, id: &str) -> Result<RatingQueue> {
        let r = sqlx::query_as::<_, QueueRow>(
            "select id, system, params, season, created_at, updated_at from musubi.rating_queues where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("rating queue".into()))?;
        to_queue(r)
    }

    pub async fn queues(&self) -> Result<Vec<RatingQueue>> {
        sqlx::query_as::<_, QueueRow>(
            "select id, system, params, season, created_at, updated_at from musubi.rating_queues order by id",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(to_queue)
        .collect()
    }

    pub async fn delete_queue(&self, id: &str) -> Result<()> {
        let r = sqlx::query("delete from musubi.rating_queues where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("rating queue".into()));
        }
        Ok(())
    }

    /// Rating of one player in the current season (the initial rating when they never played).
    pub async fn get(&self, queue: &str, account: AccountId) -> Result<Rating> {
        let q = self.queue(queue).await?;
        let r = sqlx::query_as::<_, RatingRow>(&format!(
            "select {RATING_COLS} from musubi.ratings where queue_id = $1 and season = $2 and account_id = $3"
        ))
        .bind(&q.id)
        .bind(q.season)
        .bind(account.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(match r {
            Some(r) => to_rating(&q, r),
            None => fresh(&q, account),
        })
    }

    /// Display ratings of several players (missing = never played → None), for the matchmaker.
    pub async fn display_many(
        &self,
        queue: &str,
        accounts: &[AccountId],
    ) -> Result<HashMap<AccountId, f64>> {
        let q = match self.queue(queue).await {
            Ok(q) => q,
            Err(Error::NotFound(_)) => return Ok(HashMap::new()),
            Err(e) => return Err(e),
        };
        let ids: Vec<Uuid> = accounts.iter().map(|a| a.0).collect();
        let rows = sqlx::query_as::<_, RatingRow>(&format!(
            "select {RATING_COLS} from musubi.ratings where queue_id = $1 and season = $2 and account_id = any($3)"
        ))
        .bind(&q.id)
        .bind(q.season)
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let rt = to_rating(&q, r);
                (rt.account_id, rt.display)
            })
            .collect())
    }

    /// Ladder of the current season by conservative rating; only placed players.
    pub async fn top(&self, queue: &str, limit: usize) -> Result<Vec<Rating>> {
        let q = self.queue(queue).await?;
        let order = match q.system {
            RatingSystem::Elo => "mu desc",
            RatingSystem::Glicko2 => "(mu - 2 * sigma) desc",
            RatingSystem::Openskill => "(mu - 3 * sigma) desc",
        };
        let rows = sqlx::query_as::<_, RatingRow>(&format!(
            "select {RATING_COLS} from musubi.ratings
             where queue_id = $1 and season = $2 and games >= $3
             order by {order}, account_id limit $4"
        ))
        .bind(&q.id)
        .bind(q.season)
        .bind(q.params.placement_games as i32)
        .bind(limit.clamp(1, 1000) as i64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|r| to_rating(&q, r)).collect())
    }

    /// Apply a finished match: all participants are locked and updated in one transaction.
    pub async fn apply(&self, queue: &str, res: MatchResult) -> Result<ResultApplied> {
        let q = self.queue(queue).await?;
        if res.teams.len() < 2 || res.teams.iter().any(|t| t.is_empty()) {
            return Err(Error::InvalidArgument(
                "need at least two non-empty teams".into(),
            ));
        }
        if res.teams.len() > MAX_RESULT_TEAMS
            || res.teams.iter().map(Vec::len).sum::<usize>() > MAX_RESULT_PLAYERS
        {
            return Err(Error::InvalidArgument(format!(
                "at most {MAX_RESULT_TEAMS} teams and {MAX_RESULT_PLAYERS} players per result"
            )));
        }
        let ranks: Vec<u32> = match &res.ranks {
            Some(r) if r.len() == res.teams.len() => r.clone(),
            Some(_) => {
                return Err(Error::InvalidArgument(
                    "ranks must have one entry per team".into(),
                ))
            }
            None => (1..=res.teams.len() as u32).collect(),
        };
        let mut all: Vec<AccountId> = res.teams.iter().flatten().copied().collect();
        let n = all.len();
        all.sort_by_key(|a| a.0);
        all.dedup();
        if all.len() != n {
            return Err(Error::InvalidArgument(
                "a player cannot be in two teams".into(),
            ));
        }
        let ids: Vec<Uuid> = all.iter().map(|a| a.0).collect();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        // rows first so the locks below have something to hold
        sqlx::query(
            "insert into musubi.ratings (queue_id, season, account_id, mu, sigma, vol)
             select $1, $2, a, $3, $4, $5 from unnest($6::uuid[]) as a
             on conflict do nothing",
        )
        .bind(&q.id)
        .bind(q.season)
        .bind(q.system.initial().mu)
        .bind(q.system.initial().sigma)
        .bind(q.system.initial().vol)
        .bind(&ids)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let rows = sqlx::query_as::<_, RatingRow>(&format!(
            "select {RATING_COLS} from musubi.ratings
             where queue_id = $1 and season = $2 and account_id = any($3)
             order by account_id for update"
        ))
        .bind(&q.id)
        .bind(q.season)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let current: HashMap<Uuid, RatingRow> = rows.into_iter().map(|r| (r.0, r)).collect();
        let skill_of = |a: &AccountId| {
            let r = &current[&a.0];
            Skill {
                mu: r.1,
                sigma: r.2,
                vol: r.3,
            }
        };
        let teams: Vec<Vec<Skill>> = res
            .teams
            .iter()
            .map(|t| t.iter().map(skill_of).collect())
            .collect();
        let after = match q.system {
            RatingSystem::Elo => {
                let k: Vec<Vec<f64>> = res
                    .teams
                    .iter()
                    .map(|t| {
                        t.iter()
                            .map(|a| {
                                let games = current[&a.0].4 as u32;
                                if games < q.params.placement_games {
                                    q.params.elo_k * q.params.placement_k_mult
                                } else {
                                    q.params.elo_k
                                }
                            })
                            .collect()
                    })
                    .collect();
                elo(&teams, &ranks, &k)
            }
            RatingSystem::Glicko2 => glicko2(&teams, &ranks, q.params.glicko_tau),
            RatingSystem::Openskill => openskill(
                &teams,
                &ranks,
                q.params.openskill_beta.unwrap_or(OPENSKILL_SIGMA / 2.0),
            ),
        };
        let best = *ranks.iter().min().unwrap();
        let tied_best = ranks.iter().filter(|&&r| r == best).count() > 1;
        let mut changes = Vec::with_capacity(n);
        let mut deltas = serde_json::Map::new();
        for (ti, team) in res.teams.iter().enumerate() {
            let (w, l, d) = if ranks[ti] == best {
                if tied_best {
                    (0, 0, 1)
                } else {
                    (1, 0, 0)
                }
            } else {
                (0, 1, 0)
            };
            for (mi, a) in team.iter().enumerate() {
                let before = skill_of(a);
                let s = after[ti][mi];
                let row = sqlx::query_as::<_, RatingRow>(&format!(
                    "update musubi.ratings set mu = $4, sigma = $5, vol = $6, games = games + 1,
                        wins = wins + $7, losses = losses + $8, draws = draws + $9,
                        last_played = now(), updated_at = now()
                     where queue_id = $1 and season = $2 and account_id = $3
                     returning {RATING_COLS}"
                ))
                .bind(&q.id)
                .bind(q.season)
                .bind(a.0)
                .bind(s.mu)
                .bind(s.sigma)
                .bind(s.vol)
                .bind(w)
                .bind(l)
                .bind(d)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
                let rating = to_rating(&q, row);
                deltas.insert(
                    a.0.to_string(),
                    serde_json::json!(rating.display - q.system.display(&before)),
                );
                changes.push(RatingChange {
                    account_id: *a,
                    team: ti as u32,
                    before: q.system.display(&before),
                    after: rating.display,
                    rating,
                });
            }
        }
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into musubi.rating_matches (id, queue_id, season, match_id, teams, ranks, deltas)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(&q.id)
        .bind(q.season)
        .bind(&res.match_id)
        .bind(serde_json::to_value(&res.teams).map_err(Error::internal)?)
        .bind(serde_json::to_value(&ranks).map_err(Error::internal)?)
        .bind(serde_json::Value::Object(deltas))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(ResultApplied {
            id,
            queue_id: q.id,
            season: q.season,
            changes,
        })
    }

    /// Start a new season: every rating of the old season is carried over softly.
    pub async fn new_season(&self, queue: &str, carry: Option<f64>) -> Result<RatingQueue> {
        let q = self.queue(queue).await?;
        let carry = carry.unwrap_or(q.params.season_carry);
        if !(0.0..=1.0).contains(&carry) {
            return Err(Error::InvalidArgument("carry must be in 0..=1".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows = sqlx::query_as::<_, RatingRow>(&format!(
            "select {RATING_COLS} from musubi.ratings where queue_id = $1 and season = $2"
        ))
        .bind(&q.id)
        .bind(q.season)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let next = q.season + 1;
        for r in rows {
            let s = soft_reset(
                q.system,
                carry,
                &Skill {
                    mu: r.1,
                    sigma: r.2,
                    vol: r.3,
                },
            );
            sqlx::query(
                "insert into musubi.ratings (queue_id, season, account_id, mu, sigma, vol)
                 values ($1, $2, $3, $4, $5, $6) on conflict do nothing",
            )
            .bind(&q.id)
            .bind(next)
            .bind(r.0)
            .bind(s.mu)
            .bind(s.sigma)
            .bind(s.vol)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let row = sqlx::query_as::<_, QueueRow>(
            "update musubi.rating_queues set season = $2, updated_at = now() where id = $1
             returning id, system, params, season, created_at, updated_at",
        )
        .bind(&q.id)
        .bind(next)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        to_queue(row)
    }

    /// Apply due decay steps to inactive players of one queue; returns how many ratings moved.
    /// A player idle for `decay_after + n × decay_every` has taken `n + 1` steps; `decayed_at`
    /// remembers the last step so a sweep is idempotent however often it runs.
    pub async fn decay(&self, queue: &str) -> Result<u64> {
        let q = self.queue(queue).await?;
        let p = &q.params;
        if p.decay_every_secs == 0 {
            return Ok(0);
        }
        let now = OffsetDateTime::now_utc();
        let cutoff = now - time::Duration::seconds(p.decay_after_secs as i64);
        let rows =
            sqlx::query_as::<_, (Uuid, f64, f64, f64, OffsetDateTime, Option<OffsetDateTime>)>(
                "select account_id, mu, sigma, vol, last_played, decayed_at from musubi.ratings
             where queue_id = $1 and season = $2 and last_played is not null and last_played <= $3
               and (decayed_at is null or decayed_at <= $4)",
            )
            .bind(&q.id)
            .bind(q.season)
            .bind(cutoff)
            .bind(now - time::Duration::seconds(p.decay_every_secs as i64))
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        let mut moved = 0;
        for (account, mu, sigma, vol, last_played, decayed_at) in rows {
            let since = decayed_at.unwrap_or(
                last_played + time::Duration::seconds(p.decay_after_secs as i64)
                    - time::Duration::seconds(p.decay_every_secs as i64),
            );
            let steps = ((now - since).whole_seconds() / p.decay_every_secs as i64).max(0) as u32;
            if steps == 0 {
                continue;
            }
            let mut s = Skill { mu, sigma, vol };
            for _ in 0..steps.min(52) {
                s = decay_step(q.system, p, &s);
            }
            let stamp = since + time::Duration::seconds((steps as i64) * p.decay_every_secs as i64);
            let r = sqlx::query(
                "update musubi.ratings set mu = $4, sigma = $5, decayed_at = $6, updated_at = now()
                 where queue_id = $1 and season = $2 and account_id = $3",
            )
            .bind(&q.id)
            .bind(q.season)
            .bind(account)
            .bind(s.mu)
            .bind(s.sigma)
            .bind(stamp)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            moved += r.rows_affected();
        }
        Ok(moved)
    }

    pub async fn decay_all(&self) -> Result<u64> {
        let mut n = 0;
        for q in self.queues().await? {
            n += self.decay(&q.id).await?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(mu: f64, sigma: f64, vol: f64) -> Skill {
        Skill { mu, sigma, vol }
    }

    #[test]
    fn elo_is_zero_sum_and_favours_upsets() {
        let t = vec![vec![s(1500.0, 0.0, 0.0)], vec![s(1700.0, 0.0, 0.0)]];
        let k = vec![vec![32.0], vec![32.0]];
        let out = elo(&t, &[1, 2], &k);
        let gain = out[0][0].mu - 1500.0;
        let loss = 1700.0 - out[1][0].mu;
        assert!((gain - loss).abs() < 1e-9);
        assert!(gain > 16.0 && gain < 32.0, "{gain}");
        // draw between equals changes nothing; three-way 1v1v1
        let eq = elo(
            &[vec![s(1500.0, 0.0, 0.0)], vec![s(1500.0, 0.0, 0.0)]],
            &[1, 1],
            &k,
        );
        assert_eq!(eq[0][0].mu, 1500.0);
        let k3 = vec![vec![32.0]; 3];
        let three = elo(&vec![vec![s(1500.0, 0.0, 0.0)]; 3], &[1, 2, 3], &k3);
        assert!(three[0][0].mu > 1500.0 && three[2][0].mu < 1500.0);
        assert!((three[1][0].mu - 1500.0).abs() < 1e-9);
    }

    #[test]
    fn glicko2_matches_glickman_example_per_opponent() {
        // Glickman's worked example as three one-game periods is not identical to one period
        // with three games, so check the single-game shape: a 1500/200 player beating a 1400/30
        // player gains, RD shrinks, volatility stays near 0.06.
        let out = glicko2(
            &[vec![s(1500.0, 200.0, 0.06)], vec![s(1400.0, 30.0, 0.06)]],
            &[1, 2],
            0.5,
        );
        let w = out[0][0];
        assert!(w.mu > 1540.0 && w.mu < 1600.0, "{w:?}");
        assert!(w.sigma < 200.0 && w.sigma > 150.0, "{w:?}");
        assert!((w.vol - 0.06).abs() < 0.001, "{w:?}");
        // the opponent's small RD moves little
        let l = out[1][0];
        assert!(l.mu < 1400.0 && l.mu > 1390.0, "{l:?}");
    }

    #[test]
    fn glicko2_paper_rating_period() {
        // Glickman (2013) example: 1500/200/0.06 vs 1400/30 (win), 1550/100 (loss), 1700/300 (loss)
        // → 1464.06 / 151.52 / 0.05999. Our composite-opponent step cannot replay it exactly, so
        // run the underlying per-opponent sums directly.
        let mu = 0.0;
        let phi: f64 = 200.0 / GLICKO_SCALE;
        let g = |phi: f64| 1.0 / (1.0 + 3.0 * phi * phi / std::f64::consts::PI.powi(2)).sqrt();
        let opps = [
            (1400.0, 30.0, 1.0),
            (1550.0, 100.0, 0.0),
            (1700.0, 300.0, 0.0),
        ];
        let mut v_inv = 0.0;
        let mut d = 0.0;
        for (r, rd, sc) in opps {
            let mj = (r - 1500.0) / GLICKO_SCALE;
            let pj = rd / GLICKO_SCALE;
            let e = 1.0 / (1.0 + (-g(pj) * (mu - mj)).exp());
            v_inv += g(pj).powi(2) * e * (1.0 - e);
            d += g(pj) * (sc - e);
        }
        let v = 1.0 / v_inv;
        let delta = v * d;
        let sigma = volatility(0.06, delta, phi, v, 0.5);
        let phi_star = (phi * phi + sigma * sigma).sqrt();
        let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
        let mu_new = mu + phi_new * phi_new * d;
        assert!((1500.0 + mu_new * GLICKO_SCALE - 1464.06).abs() < 0.05);
        assert!((phi_new * GLICKO_SCALE - 151.52).abs() < 0.05);
        assert!((sigma - 0.05999).abs() < 0.0001);
    }

    #[test]
    fn openskill_moves_mu_and_shrinks_sigma() {
        let init = RatingSystem::Openskill.initial();
        let out = openskill(&[vec![init], vec![init]], &[1, 2], OPENSKILL_SIGMA / 2.0);
        let (w, l) = (out[0][0], out[1][0]);
        assert!((w.mu - 27.63).abs() < 0.05, "{w:?}");
        assert!((l.mu - 22.36).abs() < 0.05, "{l:?}");
        assert!(
            w.sigma < init.sigma && (w.sigma - 8.065).abs() < 0.01,
            "{w:?}"
        );
        // 2v2: the less certain teammate takes more of the team's move
        let team = vec![s(25.0, 8.333, 0.0), s(25.0, 4.0, 0.0)];
        let out = openskill(&[team.clone(), team], &[1, 2], OPENSKILL_SIGMA / 2.0);
        assert!(out[0][0].mu - 25.0 > out[0][1].mu - 25.0);
        assert_eq!(RatingSystem::Openskill.display(&init), 1500.0);
    }

    #[test]
    fn decay_and_soft_reset() {
        let p = RatingParams::default();
        let e = decay_step(RatingSystem::Elo, &p, &s(1510.0, 0.0, 0.0));
        assert_eq!(e.mu, 1500.0, "never below the floor");
        let e = decay_step(RatingSystem::Elo, &p, &s(1490.0, 0.0, 0.0));
        assert_eq!(e.mu, 1490.0, "already below the floor: untouched");
        let g = decay_step(RatingSystem::Glicko2, &p, &s(1800.0, 60.0, 0.06));
        assert!(g.sigma > 60.0 && g.mu == 1800.0);
        let r = soft_reset(RatingSystem::Elo, 0.5, &s(1900.0, 0.0, 0.0));
        assert_eq!(r.mu, 1700.0);
        let r = soft_reset(RatingSystem::Openskill, 0.5, &s(35.0, 3.0, 0.0));
        assert_eq!(r.mu, 30.0);
        assert!(r.sigma > 3.0);
    }
}
