use std::sync::Arc;
use std::time::Duration;

use musubi_core::{AccountId, SessionId};
use musubi_matchmaker::{
    AddTicket, Candidate, CandidateMatch, Matchmaker, MmrRule, PingRule, QueueConfig, Scorer,
    Teams, TicketWire,
};
use musubi_proto::UserPresence;

fn user(mmr: f64, extra: &[(&str, &str)], pings: &[(&str, f64)]) -> Candidate {
    let mut strings: std::collections::HashMap<String, String> =
        [("queue".to_string(), "ranked".to_string())].into();
    for (k, v) in extra {
        strings.insert(k.to_string(), v.to_string());
    }
    let mut numbers: std::collections::HashMap<String, f64> = [("mmr".to_string(), mmr)].into();
    for (r, p) in pings {
        numbers.insert(format!("ping_{r}"), *p);
    }
    Candidate {
        presence: UserPresence {
            account_id: AccountId::new(),
            session_id: SessionId::new(),
            username: "u".into(),
            status: None,
            rich: None,
            invisible: false,
        },
        string_properties: strings,
        numeric_properties: numbers,
        team: None,
    }
}

fn ticket(members: Vec<Candidate>) -> AddTicket {
    AddTicket {
        party_id: None,
        members,
        query: "*".into(),
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
    }
}

fn ranked(min: usize, max: usize, teams: Option<Teams>) -> QueueConfig {
    QueueConfig {
        name: "ranked".into(),
        min_count: min,
        max_count: max,
        teams,
        mmr: Some(MmrRule::default()),
        ..Default::default()
    }
}

#[test]
fn unknown_queue_is_refused_and_queue_sizes_override_the_ticket() {
    let mm = Matchmaker::new(Duration::ZERO);
    let err = mm.add(ticket(vec![user(1000.0, &[], &[])])).unwrap_err();
    assert!(err.to_string().contains("ranked"), "{err}");
    mm.set_queue(ranked(4, 4, None)).unwrap();
    for _ in 0..3 {
        mm.add(ticket(vec![user(1000.0, &[], &[])])).unwrap();
    }
    assert!(mm.process().is_empty(), "queue wants 4, not the ticket's 2");
    mm.add(ticket(vec![user(1010.0, &[], &[])])).unwrap();
    let m = mm.process();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].queue.as_deref(), Some("ranked"));
    assert_eq!(m[0].tickets.len(), 4);
    assert!(m[0].quality.unwrap().score > 0.9);
    assert_eq!(mm.ticket_count(), 0);
}

#[test]
fn mmr_window_widens_with_wait() {
    let mm = Matchmaker::new(Duration::ZERO);
    mm.set_queue(ranked(2, 2, None)).unwrap();
    mm.add(ticket(vec![user(1000.0, &[], &[])])).unwrap();
    mm.add(ticket(vec![user(1120.0, &[], &[])])).unwrap();
    assert!(mm.process().is_empty(), "120 apart, window is 50");
    let rej = mm.take_rejections();
    assert!(
        rej.iter().any(|(q, r, _)| q == "ranked" && *r == "mmr"),
        "{rej:?}"
    );
    // a replica that already waited 30 s widens the window to 125
    let old = TicketWire {
        id: "old".into(),
        party_id: None,
        queue: Some("ranked".into()),
        members: vec![user(1240.0, &[], &[])],
        query: "*".into(),
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
        age_ms: 30_000,
    };
    mm.add_replica(old).unwrap();
    let m = mm.process();
    assert_eq!(m.len(), 1, "patient ticket takes the closest partner");
    let mmrs: Vec<f64> = m[0]
        .tickets
        .iter()
        .map(|t| t.members[0].numeric_properties["mmr"])
        .collect();
    assert!(mmrs.contains(&1240.0) && mmrs.contains(&1120.0), "{mmrs:?}");
}

#[test]
fn teams_are_balanced_parties_stay_together_and_roles_are_capped() {
    let mm = Matchmaker::new(Duration::ZERO);
    let mut q = ranked(4, 4, Some(Teams { count: 2, size: 2 }));
    q.mmr = Some(MmrRule {
        initial_delta: 1000.0,
        ..MmrRule::default()
    });
    q.roles.insert("healer".into(), 1);
    mm.set_queue(q).unwrap();
    let a = user(1400.0, &[("role", "healer")], &[]);
    let b = user(1000.0, &[("role", "healer")], &[]);
    let party = ticket(vec![
        user(1200.0, &[("role", "dps")], &[]),
        user(1200.0, &[("role", "dps")], &[]),
    ]);
    mm.add(ticket(vec![a])).unwrap();
    mm.add(ticket(vec![b])).unwrap();
    mm.add(party).unwrap();
    // party (2400) must be one team; the two healers can't share a team anyway → infeasible
    assert!(mm.process().is_empty());
    assert!(mm.take_rejections().iter().any(|(_, r, _)| *r == "roles"));
    // four solos: healers must split, so the best split is 1400h+1100d vs 1000h+1300d
    let ids: Vec<String> = mm.waiting().keys().cloned().collect();
    assert_eq!(ids, vec!["ranked".to_string()]);
    let mm = Matchmaker::new(Duration::ZERO);
    let mut q = ranked(4, 4, Some(Teams { count: 2, size: 2 }));
    q.mmr = Some(MmrRule {
        initial_delta: 1000.0,
        ..MmrRule::default()
    });
    q.roles.insert("healer".into(), 1);
    mm.set_queue(q).unwrap();
    for u in [
        user(1400.0, &[("role", "healer")], &[]),
        user(1000.0, &[("role", "healer")], &[]),
        user(1300.0, &[("role", "dps")], &[]),
        user(1100.0, &[("role", "dps")], &[]),
    ] {
        mm.add(ticket(vec![u])).unwrap();
    }
    let m = mm.process();
    assert_eq!(m.len(), 1);
    let mut teams = [Vec::new(), Vec::new()];
    for t in &m[0].tickets {
        let u = &t.members[0];
        teams[u.team.unwrap() as usize].push((
            u.numeric_properties["mmr"],
            u.string_properties["role"].clone(),
        ));
    }
    for team in &teams {
        assert_eq!(team.len(), 2);
        assert_eq!(
            team.iter().filter(|(_, r)| r == "healer").count(),
            1,
            "{teams:?}"
        );
    }
    let mut sums: Vec<f64> = teams
        .iter()
        .map(|t| t.iter().map(|(m, _)| m).sum())
        .collect();
    sums.sort_by(f64::total_cmp);
    assert_eq!(sums, vec![2300.0, 2500.0], "{teams:?}");
    assert_eq!(m[0].quality.unwrap().team_mmr_gap, 100.0);
}

#[test]
fn ping_region_crossplay_avoid_and_no_repeat() {
    let mm = Matchmaker::new(Duration::ZERO);
    let mut q = ranked(2, 2, None);
    q.mmr = None;
    q.ping = Some(PingRule::default());
    q.crossplay = vec![vec!["pc".into(), "xbox".into()]];
    q.no_repeat_secs = 600;
    mm.set_queue(q).unwrap();
    let eu = |p: &str| user(0.0, &[("platform", p)], &[("eu", 20.0), ("us", 110.0)]);
    let us = |p: &str| user(0.0, &[("platform", p)], &[("eu", 130.0), ("us", 15.0)]);
    mm.add(ticket(vec![eu("pc")])).unwrap();
    mm.add(ticket(vec![us("pc")])).unwrap();
    assert!(mm.process().is_empty(), "no region under 40 ms for both");
    mm.add(ticket(vec![eu("switch")])).unwrap();
    assert!(
        mm.process().is_empty(),
        "switch is not in the pc/xbox group"
    );
    let x = eu("xbox");
    let x_id = x.presence.account_id;
    mm.add(ticket(vec![x])).unwrap();
    let m = mm.process();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].region.as_deref(), Some("eu"));
    let players: Vec<AccountId> = m[0]
        .tickets
        .iter()
        .map(|t| t.members[0].presence.account_id)
        .collect();
    assert!(players.contains(&x_id));
    let pc = players.into_iter().find(|p| *p != x_id).unwrap();
    // the same two cannot meet again within no_repeat_secs
    let mut again = eu("xbox");
    again.presence.account_id = x_id;
    let mut again_pc = eu("pc");
    again_pc.presence.account_id = pc;
    mm.add(ticket(vec![again])).unwrap();
    mm.add(ticket(vec![again_pc])).unwrap();
    assert!(mm.process().is_empty(), "recent opponents are kept apart");
    assert!(mm.take_rejections().iter().any(|(_, r, _)| *r == "recent"));
    // avoid list keeps a third player away from the xbox one
    let mut hater = eu("pc");
    hater
        .string_properties
        .insert("avoid".into(), x_id.0.to_string());
    mm.add(ticket(vec![hater])).unwrap();
    let m = mm.process();
    assert_eq!(
        m.len(),
        1,
        "hater pairs with the pc player, not the avoided one"
    );
    assert!(m[0]
        .tickets
        .iter()
        .all(|t| t.members[0].presence.account_id != x_id));
}

struct Veto;
impl Scorer for Veto {
    fn score(&self, c: &CandidateMatch) -> Option<f32> {
        Some(if c.quality.mmr_spread > 10.0 {
            -1.0
        } else {
            0.42
        })
    }
}

struct Pairs;
impl Scorer for Pairs {
    fn propose(&self, _q: &QueueConfig, t: &[TicketWire]) -> Option<Vec<Vec<String>>> {
        // pair the highest with the lowest, ignoring the built-in closeness rule
        let mut ids: Vec<(f64, String)> = t
            .iter()
            .map(|w| (w.members[0].numeric_properties["mmr"], w.id.clone()))
            .collect();
        ids.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some(vec![vec![ids[0].1.clone(), ids[ids.len() - 1].1.clone()]])
    }
}

#[test]
fn scorer_can_veto_regrade_or_propose() {
    let mm = Matchmaker::new(Duration::ZERO).with_scorer(Arc::new(Veto));
    mm.set_queue(ranked(2, 2, None)).unwrap();
    mm.add(ticket(vec![user(1000.0, &[], &[])])).unwrap();
    mm.add(ticket(vec![user(1030.0, &[], &[])])).unwrap();
    assert!(mm.process().is_empty(), "vetoed by the scorer");
    assert!(mm.take_rejections().iter().any(|(_, r, _)| *r == "vetoed"));
    mm.add(ticket(vec![user(1005.0, &[], &[])])).unwrap();
    let m = mm.process();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].quality.unwrap().score, 0.42);

    let mm = Matchmaker::new(Duration::ZERO).with_scorer(Arc::new(Pairs));
    let mut q = ranked(2, 2, None);
    q.mmr = None;
    mm.set_queue(q).unwrap();
    for mmr in [1000.0, 1500.0, 2000.0] {
        mm.add(ticket(vec![user(mmr, &[], &[])])).unwrap();
    }
    let m = mm.process();
    assert_eq!(m.len(), 1);
    let mut mmrs: Vec<f64> = m[0]
        .tickets
        .iter()
        .map(|t| t.members[0].numeric_properties["mmr"])
        .collect();
    mmrs.sort_by(f64::total_cmp);
    assert_eq!(mmrs, vec![1000.0, 2000.0]);
    assert_eq!(mm.ticket_count(), 1);
}

#[test]
fn free_tickets_and_queue_tickets_never_mix() {
    let mm = Matchmaker::new(Duration::ZERO);
    mm.set_queue(ranked(2, 2, None)).unwrap();
    mm.add(ticket(vec![user(1000.0, &[], &[])])).unwrap();
    let mut free = user(1000.0, &[], &[]);
    free.string_properties.remove("queue");
    mm.add(ticket(vec![free])).unwrap();
    assert!(mm.process().is_empty());
    assert_eq!(mm.waiting().get("ranked"), Some(&1));
    assert_eq!(mm.waiting().get(""), Some(&1));
}
