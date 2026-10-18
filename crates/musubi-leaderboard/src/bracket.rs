//! Tournament brackets: single / double elimination with standard seeding and byes, and Swiss
//! rounds paired by score without rematches. The bracket is one document per tournament; the
//! algorithms are pure and the service stores the document as a single JSON row.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::TournamentService;

pub const MAX_PARTICIPANTS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Single,
    Double,
    Swiss,
    /// Everyone plays everyone once; all rounds exist from the start (circle method).
    RoundRobin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Winners,
    Losers,
    Final,
}

/// What sits in one half of a match: nobody yet, a bye (the other side advances), or a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Empty,
    Bye,
    Player(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    pub seed: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub side: Side,
    pub round: i32,
    pub slot: i32,
    /// 0 → `a`, 1 → `b`.
    pub pos: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    pub id: Uuid,
    pub side: Side,
    pub round: i32,
    pub slot: i32,
    pub a: Slot,
    pub b: Slot,
    pub winner: Option<Uuid>,
    /// Resolved without play (one side was a bye).
    #[serde(default)]
    pub bye: bool,
    /// End-game report (`/v1/server/matches/{id}/results`) that decided it, if any.
    #[serde(default)]
    pub match_id: Option<String>,
    #[serde(default)]
    pub score: Value,
    pub winner_to: Option<Link>,
    pub loser_to: Option<Link>,
    /// Unix time by which a ready match must be reported (`Bracket::deadline_secs` after it
    /// became ready); past it the organiser may `forfeit` the absent side.
    #[serde(default)]
    pub deadline: Option<i64>,
}

impl Match {
    pub fn ready(&self) -> bool {
        self.winner.is_none()
            && !self.bye
            && matches!(self.a, Slot::Player(_))
            && matches!(self.b, Slot::Player(_))
    }

    pub fn status(&self) -> &'static str {
        if self.winner.is_some() {
            "done"
        } else if self.bye {
            "void"
        } else if self.ready() {
            "ready"
        } else {
            "pending"
        }
    }

    fn players(&self) -> (Option<Uuid>, Option<Uuid>) {
        let p = |s: Slot| match s {
            Slot::Player(id) => Some(id),
            _ => None,
        };
        (p(self.a), p(self.b))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bracket {
    pub format: Format,
    pub participants: Vec<Participant>,
    pub matches: Vec<Match>,
    /// Swiss: rounds generated so far. Elimination: number of winners-bracket rounds.
    pub round: i32,
    /// Swiss: rounds planned. Elimination: number of winners-bracket rounds.
    pub rounds: i32,
    pub champion: Option<Uuid>,
    /// Seconds a ready match has before it counts as a no-show candidate (None = no deadline).
    #[serde(default)]
    pub deadline_secs: Option<i64>,
}

fn now_secs() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Give every ready, undecided match without a deadline one, counting from now.
pub fn stamp_deadlines(b: &mut Bracket) {
    let Some(secs) = b.deadline_secs else { return };
    let due = now_secs() + secs.max(0);
    for m in &mut b.matches {
        if m.ready() && m.winner.is_none() && m.deadline.is_none() {
            m.deadline = Some(due);
        }
    }
}

/// The absent side loses: the other player wins with `score = {"forfeit": absent}`. Allowed
/// only once the match's deadline has passed (or when it has none).
pub fn forfeit(b: &mut Bracket, match_id: Uuid, absent: Uuid) -> Result<Vec<Uuid>> {
    let Some(m) = b.matches.iter().find(|m| m.id == match_id) else {
        return Err(Error::NotFound(format!("bracket match {match_id}")));
    };
    if let Some(d) = m.deadline {
        if now_secs() < d {
            return Err(Error::Conflict(format!(
                "match {match_id} is not overdue until {d}"
            )));
        }
    }
    let (a, bb) = m.players();
    let winner = match (a, bb) {
        (Some(x), Some(y)) if x == absent => y,
        (Some(x), Some(y)) if y == absent => x,
        _ => {
            return Err(Error::InvalidArgument(format!(
                "{absent} is not playing in match {match_id}"
            )))
        }
    };
    report(
        b,
        match_id,
        winner,
        serde_json::json!({ "forfeit": absent }),
        None,
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Standing {
    pub id: Uuid,
    pub name: String,
    pub seed: i32,
    pub wins: i32,
    pub losses: i32,
    pub byes: i32,
    /// Sum of beaten/drawn opponents' wins (Buchholz) — Swiss tiebreak.
    pub buchholz: i32,
    pub eliminated: bool,
}

/// Standard seeding order for a power-of-two bracket: seed 1 meets the lowest seed, the two best
/// seeds can only meet in the final (`[0, 7, 3, 4, 1, 6, 2, 5]` for 8).
pub fn seed_order(size: usize) -> Vec<usize> {
    let mut v = vec![0usize];
    while v.len() < size {
        let n = v.len() * 2;
        let mut next = Vec::with_capacity(n);
        for s in &v {
            next.push(*s);
            next.push(n - 1 - *s);
        }
        v = next;
    }
    v
}

fn log2(size: usize) -> i32 {
    size.trailing_zeros() as i32
}

/// Build a bracket for `participants` (already in seed order).
pub fn generate(
    format: Format,
    participants: Vec<Participant>,
    swiss_rounds: Option<i32>,
) -> Result<Bracket> {
    let n = participants.len();
    if n < 2 {
        return Err(Error::InvalidArgument(
            "a bracket needs at least 2 participants".into(),
        ));
    }
    if n > MAX_PARTICIPANTS {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_PARTICIPANTS} participants"
        )));
    }
    let size = n.next_power_of_two();
    let r = log2(size);
    let mut b = Bracket {
        format,
        participants,
        matches: Vec::new(),
        round: 0,
        rounds: r,
        champion: None,
        deadline_secs: None,
    };
    match format {
        Format::Swiss => {
            let default_rounds = log2(size);
            b.rounds = swiss_rounds.unwrap_or(default_rounds).clamp(1, 20);
            swiss_next_round(&mut b)?;
            return Ok(b);
        }
        Format::RoundRobin => {
            // circle method: fix the first seat, rotate the rest; odd field → one bye a round
            let mut seats: Vec<Slot> = b.participants.iter().map(|p| Slot::Player(p.id)).collect();
            if seats.len() % 2 == 1 {
                seats.push(Slot::Bye);
            }
            let k = seats.len();
            b.rounds = (k - 1) as i32;
            for round in 1..=b.rounds {
                for i in 0..k / 2 {
                    let (a, bb) = (seats[i], seats[k - 1 - i]);
                    if a == Slot::Bye || bb == Slot::Bye {
                        continue;
                    }
                    b.matches.push(Match {
                        id: Uuid::now_v7(),
                        side: Side::Winners,
                        round,
                        slot: i as i32,
                        a,
                        b: bb,
                        winner: None,
                        bye: false,
                        match_id: None,
                        score: Value::Null,
                        winner_to: None,
                        loser_to: None,
                        deadline: None,
                    });
                }
                let last = seats.pop().unwrap();
                seats.insert(1, last);
            }
            b.round = b.rounds;
            return Ok(b);
        }
        Format::Single | Format::Double => {}
    }
    let order = seed_order(size);
    let slot_of = |idx: usize| match b.participants.get(idx) {
        Some(p) => Slot::Player(p.id),
        None => Slot::Bye,
    };
    for round in 1..=r {
        let count = size >> round;
        for i in 0..count {
            let (a, bb) = if round == 1 {
                (slot_of(order[2 * i]), slot_of(order[2 * i + 1]))
            } else {
                (Slot::Empty, Slot::Empty)
            };
            let winner_to = if round < r {
                Some(Link {
                    side: Side::Winners,
                    round: round + 1,
                    slot: (i / 2) as i32,
                    pos: (i % 2) as u8,
                })
            } else if format == Format::Double {
                Some(Link {
                    side: Side::Final,
                    round: 1,
                    slot: 0,
                    pos: 0,
                })
            } else {
                None
            };
            let loser_to = if format == Format::Double && r > 1 {
                Some(if round == 1 {
                    Link {
                        side: Side::Losers,
                        round: 1,
                        slot: (i / 2) as i32,
                        pos: (i % 2) as u8,
                    }
                } else {
                    // alternate drop-in order per round so early rematches are rarer
                    let slot = if round % 2 == 0 { count - 1 - i } else { i };
                    Link {
                        side: Side::Losers,
                        round: 2 * (round - 1),
                        slot: slot as i32,
                        pos: 1,
                    }
                })
            } else {
                None
            };
            b.matches.push(Match {
                id: Uuid::now_v7(),
                side: Side::Winners,
                round,
                slot: i as i32,
                a,
                b: bb,
                winner: None,
                bye: false,
                match_id: None,
                score: Value::Null,
                winner_to,
                loser_to,
                deadline: None,
            });
        }
    }
    if format == Format::Double {
        if r == 1 {
            // two players: the "double" bracket is a single match
            b.matches[0].winner_to = None;
        } else {
            let lb_rounds = 2 * (r - 1);
            for k in 1..=lb_rounds {
                let j = (k + 1) / 2; // 1-based pair index: rounds 2j-1 and 2j share a size
                let count = size >> (j + 1);
                for i in 0..count {
                    let winner_to = if k == lb_rounds {
                        Link {
                            side: Side::Final,
                            round: 1,
                            slot: 0,
                            pos: 1,
                        }
                    } else if k % 2 == 1 {
                        Link {
                            side: Side::Losers,
                            round: k + 1,
                            slot: i as i32,
                            pos: 0,
                        }
                    } else {
                        Link {
                            side: Side::Losers,
                            round: k + 1,
                            slot: (i / 2) as i32,
                            pos: (i % 2) as u8,
                        }
                    };
                    b.matches.push(Match {
                        id: Uuid::now_v7(),
                        side: Side::Losers,
                        round: k,
                        slot: i as i32,
                        a: Slot::Empty,
                        b: Slot::Empty,
                        winner: None,
                        bye: false,
                        match_id: None,
                        score: Value::Null,
                        winner_to: Some(winner_to),
                        loser_to: None,
                        deadline: None,
                    });
                }
            }
            b.matches.push(Match {
                id: Uuid::now_v7(),
                side: Side::Final,
                round: 1,
                slot: 0,
                a: Slot::Empty,
                b: Slot::Empty,
                winner: None,
                bye: false,
                match_id: None,
                score: Value::Null,
                winner_to: None,
                loser_to: None,
                deadline: None,
            });
        }
    }
    b.round = r;
    resolve_byes(&mut b);
    Ok(b)
}

fn index_of(b: &Bracket, link: Link) -> Option<usize> {
    b.matches
        .iter()
        .position(|m| m.side == link.side && m.round == link.round && m.slot == link.slot)
}

fn fill(b: &mut Bracket, link: Option<Link>, slot: Slot) {
    let Some(link) = link else {
        return;
    };
    if let Some(i) = index_of(b, link) {
        if link.pos == 0 {
            b.matches[i].a = slot;
        } else {
            b.matches[i].b = slot;
        }
    }
}

fn is_final(b: &Bracket, m: &Match) -> bool {
    match b.format {
        Format::Single => m.side == Side::Winners && m.round == b.rounds,
        Format::Double => m.side == Side::Final || (b.rounds == 1 && m.side == Side::Winners),
        Format::Swiss | Format::RoundRobin => false,
    }
}

/// Walk matches with a bye on one side: the other side advances, a bye is passed down the
/// losers path, and two byes void the match. Repeats until nothing changes.
fn resolve_byes(b: &mut Bracket) {
    loop {
        let mut changed = false;
        for i in 0..b.matches.len() {
            let m = b.matches[i].clone();
            if m.winner.is_some() || m.bye {
                continue;
            }
            let (w, l) = match (m.a, m.b) {
                (Slot::Bye, Slot::Player(p)) | (Slot::Player(p), Slot::Bye) => (Some(p), None),
                (Slot::Bye, Slot::Bye) => (None, None),
                _ => continue,
            };
            b.matches[i].bye = true;
            b.matches[i].winner = w;
            let w_slot = w.map(Slot::Player).unwrap_or(Slot::Bye);
            fill(b, m.winner_to, w_slot);
            fill(b, m.loser_to, l.map(Slot::Player).unwrap_or(Slot::Bye));
            if is_final(b, &m) {
                b.champion = w;
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

fn ready_ids(b: &Bracket) -> Vec<Uuid> {
    b.matches
        .iter()
        .filter(|m| m.ready())
        .map(|m| m.id)
        .collect()
}

/// Record a result. Idempotent for the same winner; a different winner is a conflict. Returns
/// the matches that became ready because of it.
pub fn report(
    b: &mut Bracket,
    match_id: Uuid,
    winner: Uuid,
    score: Value,
    ext_match: Option<String>,
) -> Result<Vec<Uuid>> {
    let before = ready_ids(b);
    let Some(i) = b.matches.iter().position(|m| m.id == match_id) else {
        return Err(Error::NotFound(format!("bracket match {match_id}")));
    };
    let m = b.matches[i].clone();
    let (a, bb) = m.players();
    if a != Some(winner) && bb != Some(winner) {
        return Err(Error::InvalidArgument(format!(
            "{winner} is not playing in match {match_id}"
        )));
    }
    if let Some(w) = m.winner {
        if w == winner {
            return Ok(vec![]);
        }
        return Err(Error::Conflict(format!(
            "match {match_id} already decided for {w}"
        )));
    }
    if !m.ready() {
        return Err(Error::Conflict(format!(
            "match {match_id} is not ready (both players must be known)"
        )));
    }
    let loser = if a == Some(winner) { bb } else { a };
    {
        let mm = &mut b.matches[i];
        mm.winner = Some(winner);
        mm.score = score;
        mm.match_id = ext_match;
    }
    fill(b, m.winner_to, Slot::Player(winner));
    if let Some(l) = loser {
        fill(b, m.loser_to, Slot::Player(l));
    }
    resolve_byes(b);
    if is_final(b, &m) {
        b.champion = Some(winner);
    }
    if matches!(b.format, Format::Swiss | Format::RoundRobin)
        && b.round >= b.rounds
        && b.matches.iter().all(|m| m.winner.is_some() || m.bye)
    {
        b.champion = standings(b).first().map(|s| s.id);
    }
    stamp_deadlines(b);
    let after = ready_ids(b);
    Ok(after
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect())
}

/// Wins/losses per participant; Swiss adds Buchholz, elimination marks who is out.
pub fn standings(b: &Bracket) -> Vec<Standing> {
    let mut rows: Vec<Standing> = b
        .participants
        .iter()
        .map(|p| Standing {
            id: p.id,
            name: p.name.clone(),
            seed: p.seed,
            wins: 0,
            losses: 0,
            byes: 0,
            buchholz: 0,
            eliminated: false,
        })
        .collect();
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let idx = |id: Uuid| ids.iter().position(|r| *r == id);
    let mut opponents: Vec<Vec<Uuid>> = vec![vec![]; rows.len()];
    for m in &b.matches {
        let (a, bb) = m.players();
        if m.bye {
            if let (Some(p), Some(i)) = (m.winner, m.winner.and_then(idx)) {
                let _ = p;
                rows[i].byes += 1;
                rows[i].wins += 1;
            }
            continue;
        }
        let Some(w) = m.winner else { continue };
        let l = if a == Some(w) { bb } else { a };
        if let Some(i) = idx(w) {
            rows[i].wins += 1;
            if let Some(l) = l {
                opponents[i].push(l);
            }
        }
        if let Some(i) = l.and_then(idx) {
            rows[i].losses += 1;
            opponents[i].push(w);
        }
    }
    let wins: Vec<(Uuid, i32)> = rows.iter().map(|r| (r.id, r.wins)).collect();
    for (i, r) in rows.iter_mut().enumerate() {
        r.buchholz = opponents[i]
            .iter()
            .map(|o| {
                wins.iter()
                    .find(|(id, _)| id == o)
                    .map(|(_, w)| *w)
                    .unwrap_or(0)
            })
            .sum();
        r.eliminated = match b.format {
            Format::Single => r.losses >= 1,
            Format::Double => r.losses >= 2,
            Format::Swiss | Format::RoundRobin => false,
        };
    }
    rows.sort_by(|x, y| {
        y.wins
            .cmp(&x.wins)
            .then(x.losses.cmp(&y.losses))
            .then(y.buchholz.cmp(&x.buchholz))
            .then(x.seed.cmp(&y.seed))
    });
    rows
}

/// Swiss: pair the next round by standing, avoiding rematches; an odd player out gets a bye
/// (lowest-ranked without one yet). Fails while the current round is unfinished.
pub fn swiss_next_round(b: &mut Bracket) -> Result<i32> {
    if b.format != Format::Swiss {
        return Err(Error::InvalidArgument(
            "rounds are generated only for swiss brackets".into(),
        ));
    }
    if b.round >= b.rounds {
        return Err(Error::Conflict(format!(
            "all {} rounds have been played",
            b.rounds
        )));
    }
    if b.matches
        .iter()
        .any(|m| m.round == b.round && m.winner.is_none() && !m.bye)
    {
        return Err(Error::Conflict(format!(
            "round {} has unfinished matches",
            b.round
        )));
    }
    let st = standings(b);
    let mut order: Vec<Uuid> = st.iter().map(|s| s.id).collect();
    let played: Vec<(Uuid, Uuid)> = b
        .matches
        .iter()
        .filter_map(|m| match m.players() {
            (Some(a), Some(c)) => Some((a, c)),
            _ => None,
        })
        .collect();
    let met = |x: Uuid, y: Uuid| {
        played
            .iter()
            .any(|(a, c)| (*a == x && *c == y) || (*a == y && *c == x))
    };
    let round = b.round + 1;
    let mut pairs: Vec<(Uuid, Slot)> = Vec::new();
    if order.len() % 2 == 1 {
        let pos = order
            .iter()
            .rposition(|id| st.iter().find(|s| s.id == *id).is_some_and(|s| s.byes == 0))
            .unwrap_or(order.len() - 1);
        let id = order.remove(pos);
        pairs.push((id, Slot::Bye));
    }
    while !order.is_empty() {
        let a = order.remove(0);
        let pick = order.iter().position(|c| !met(a, *c)).unwrap_or(0);
        let c = order.remove(pick);
        pairs.push((a, Slot::Player(c)));
    }
    for (slot, (a, c)) in pairs.into_iter().enumerate() {
        b.matches.push(Match {
            id: Uuid::now_v7(),
            side: Side::Winners,
            round,
            slot: slot as i32,
            a: Slot::Player(a),
            b: c,
            winner: None,
            bye: false,
            match_id: None,
            score: Value::Null,
            winner_to: None,
            loser_to: None,
            deadline: None,
        });
    }
    b.round = round;
    resolve_byes(b);
    Ok(round)
}

impl TournamentService {
    pub async fn bracket_get(&self, id: &str) -> Result<Option<Bracket>> {
        let doc: Option<Value> = sqlx::query_scalar(
            "select doc from musubi.tournament_brackets where tournament_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        doc.map(|d| serde_json::from_value(d).map_err(Error::internal))
            .transpose()
    }

    pub async fn bracket_create(&self, id: &str, b: &Bracket) -> Result<()> {
        let n = sqlx::query(
            "insert into musubi.tournament_brackets (tournament_id, doc) values ($1, $2)
             on conflict (tournament_id) do nothing",
        )
        .bind(id)
        .bind(serde_json::to_value(b)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::Conflict(format!(
                "tournament {id} already has a bracket"
            )));
        }
        Ok(())
    }

    /// Load → mutate → store under a row lock, so two reports cannot race.
    pub async fn bracket_update<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Bracket) -> Result<T>,
    ) -> Result<(Bracket, T)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let doc: Option<Value> = sqlx::query_scalar(
            "select doc from musubi.tournament_brackets where tournament_id = $1 for update",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(doc) = doc else {
            return Err(Error::NotFound(format!("tournament {id} has no bracket")));
        };
        let mut b: Bracket = serde_json::from_value(doc).map_err(Error::internal)?;
        let out = f(&mut b)?;
        sqlx::query(
            "update musubi.tournament_brackets set doc = $2, updated_at = now() where tournament_id = $1",
        )
        .bind(id)
        .bind(serde_json::to_value(&b)?)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((b, out))
    }

    pub async fn bracket_delete(&self, id: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.tournament_brackets where tournament_id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn players(n: usize) -> Vec<Participant> {
        (0..n)
            .map(|i| Participant {
                id: Uuid::from_u128(1000 + i as u128),
                name: format!("p{i}"),
                seed: i as i32 + 1,
            })
            .collect()
    }

    fn pid(i: usize) -> Uuid {
        Uuid::from_u128(1000 + i as u128)
    }

    fn ready(b: &Bracket) -> Vec<(Side, i32, i32)> {
        let mut v: Vec<_> = b
            .matches
            .iter()
            .filter(|m| m.ready())
            .map(|m| (m.side, m.round, m.slot))
            .collect();
        v.sort_by_key(|k| (k.1, k.2));
        v
    }

    fn find(b: &Bracket, side: Side, round: i32, slot: i32) -> Match {
        b.matches
            .iter()
            .find(|m| m.side == side && m.round == round && m.slot == slot)
            .unwrap()
            .clone()
    }

    #[test]
    fn seeding() {
        assert_eq!(seed_order(8), vec![0, 7, 3, 4, 1, 6, 2, 5]);
        assert_eq!(seed_order(2), vec![0, 1]);
    }

    #[test]
    fn single_with_byes() {
        let mut b = generate(Format::Single, players(5), None).unwrap();
        assert_eq!(b.rounds, 3);
        assert_eq!(b.matches.len(), 7);
        // seeds 1, 2, 3 get byes (slots 8, 7, 6 are empty); 4 v 5 is the only round-1 game
        assert_eq!(b.matches.iter().filter(|m| m.bye).count(), 3);
        assert_eq!(
            ready(&b),
            vec![(Side::Winners, 1, 1), (Side::Winners, 2, 1)]
        );
        let r1 = find(&b, Side::Winners, 1, 1);
        assert_eq!((r1.a, r1.b), (Slot::Player(pid(3)), Slot::Player(pid(4))));
        // a result for a match that is not ready is refused; wrong player refused
        let sf = find(&b, Side::Winners, 2, 0);
        assert!(report(&mut b, sf.id, pid(0), Value::Null, None).is_err());
        assert!(report(&mut b, r1.id, pid(0), Value::Null, None).is_err());
        let newly = report(&mut b, r1.id, pid(3), Value::Null, Some("m1".into())).unwrap();
        assert_eq!(newly, vec![sf.id]);
        assert!(
            report(&mut b, r1.id, pid(3), Value::Null, None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        assert!(
            report(&mut b, r1.id, pid(4), Value::Null, None).is_err(),
            "conflict"
        );
        report(&mut b, sf.id, pid(0), Value::Null, None).unwrap();
        let sf2 = find(&b, Side::Winners, 2, 1);
        assert_eq!((sf2.a, sf2.b), (Slot::Player(pid(1)), Slot::Player(pid(2))));
        report(&mut b, sf2.id, pid(1), Value::Null, None).unwrap();
        let f = find(&b, Side::Winners, 3, 0);
        assert_eq!((f.a, f.b), (Slot::Player(pid(0)), Slot::Player(pid(1))));
        assert!(b.champion.is_none());
        report(
            &mut b,
            f.id,
            pid(1),
            serde_json::json!({"a": 1, "b": 2}),
            None,
        )
        .unwrap();
        assert_eq!(b.champion, Some(pid(1)));
        let st = standings(&b);
        assert_eq!(st[0].id, pid(1));
        assert!(st.iter().filter(|s| s.eliminated).count() == 4);
    }

    #[test]
    fn double_elimination_four() {
        let mut b = generate(Format::Double, players(4), None).unwrap();
        assert_eq!(b.matches.len(), 2 + 1 + 1 + 1 + 1);
        let w1 = find(&b, Side::Winners, 1, 0);
        let w2 = find(&b, Side::Winners, 1, 1);
        report(&mut b, w1.id, pid(0), Value::Null, None).unwrap();
        report(&mut b, w2.id, pid(1), Value::Null, None).unwrap();
        let l1 = find(&b, Side::Losers, 1, 0);
        assert_eq!((l1.a, l1.b), (Slot::Player(pid(3)), Slot::Player(pid(2))));
        let wf = find(&b, Side::Winners, 2, 0);
        assert!(wf.ready());
        report(&mut b, l1.id, pid(2), Value::Null, None).unwrap();
        report(&mut b, wf.id, pid(0), Value::Null, None).unwrap();
        let l2 = find(&b, Side::Losers, 2, 0);
        assert_eq!((l2.a, l2.b), (Slot::Player(pid(2)), Slot::Player(pid(1))));
        report(&mut b, l2.id, pid(1), Value::Null, None).unwrap();
        let f = find(&b, Side::Final, 1, 0);
        assert_eq!((f.a, f.b), (Slot::Player(pid(0)), Slot::Player(pid(1))));
        report(&mut b, f.id, pid(1), Value::Null, None).unwrap();
        assert_eq!(b.champion, Some(pid(1)));
        let st = standings(&b);
        assert_eq!(st[0].id, pid(1));
        assert_eq!(st.iter().find(|s| s.id == pid(3)).unwrap().losses, 2);
    }

    #[test]
    fn double_with_byes_resolves() {
        let mut b = generate(Format::Double, players(5), None).unwrap();
        // round-1 byes send "bye" down the losers path: losers round 1 has one void match
        assert!(b.matches.iter().any(|m| m.side == Side::Losers && m.bye));
        let mut guard = 0;
        while b.champion.is_none() {
            let next = b
                .matches
                .iter()
                .find(|m| m.ready())
                .cloned()
                .expect("a ready match");
            let (Some(a), _) = next.players() else {
                panic!()
            };
            report(&mut b, next.id, a, Value::Null, None).unwrap();
            guard += 1;
            assert!(guard < 32);
        }
        assert_eq!(b.champion, Some(pid(0)));
    }

    #[test]
    fn swiss_rounds() {
        let mut b = generate(Format::Swiss, players(5), Some(3)).unwrap();
        assert_eq!((b.round, b.rounds), (1, 3));
        let r1: Vec<Match> = b.matches.iter().filter(|m| m.round == 1).cloned().collect();
        assert_eq!(r1.len(), 3);
        assert_eq!(r1.iter().filter(|m| m.bye).count(), 1);
        assert!(swiss_next_round(&mut b).is_err(), "round 1 unfinished");
        for m in r1.iter().filter(|m| !m.bye) {
            let (Some(a), _) = m.players() else { panic!() };
            report(&mut b, m.id, a, Value::Null, None).unwrap();
        }
        assert_eq!(swiss_next_round(&mut b).unwrap(), 2);
        let pairs = |b: &Bracket| -> Vec<(Uuid, Uuid)> {
            b.matches
                .iter()
                .filter_map(|m| match m.players() {
                    (Some(a), Some(c)) => Some((a.min(c), a.max(c))),
                    _ => None,
                })
                .collect()
        };
        let p = pairs(&b);
        let mut dedup = p.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(p.len(), dedup.len(), "no rematch in round 2");
        let byes: Vec<Uuid> = b
            .matches
            .iter()
            .filter(|m| m.bye)
            .filter_map(|m| m.winner)
            .collect();
        assert_eq!(byes.len(), 2);
        assert_ne!(byes[0], byes[1], "a different player gets the bye");
        for round in 2..=3 {
            for m in b
                .matches
                .clone()
                .iter()
                .filter(|m| m.round == round && m.ready())
            {
                let (Some(a), _) = m.players() else { panic!() };
                report(&mut b, m.id, a, Value::Null, None).unwrap();
            }
            if round < 3 {
                swiss_next_round(&mut b).unwrap();
            }
        }
        assert!(b.champion.is_some());
        assert!(swiss_next_round(&mut b).is_err(), "all rounds played");
        let st = standings(&b);
        assert!(st[0].wins >= st[1].wins);
    }
}
