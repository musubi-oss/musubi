//! Realtime WebSocket integration test. Needs `MUSUBI_TEST_DATABASE_URL`; skipped otherwise.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use musubi_auth::AuthService;
use musubi_core::config::Config;
use musubi_proto::realtime::ChannelKind;
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage};
use musubi_server::app::AppState;
use musubi_server::realtime::Hub;
use musubi_storage::StorageService;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

/// Each test gets its own cluster group so concurrently running tests do not share a bus
/// (otherwise every node is a matchmaker leader and replicated tickets get matched twice).
fn test_group() -> String {
    format!("t-{}", uuid::Uuid::now_v7().simple())
}

async fn start() -> Option<(String, axum::Router)> {
    start_with(true, &test_group()).await
}

async fn start_in(group: &str) -> Option<(String, axum::Router)> {
    start_with(true, group).await
}

/// `leader=false` builds a node that replicates tickets but never runs matchmaker passes.
async fn start_with(leader: bool, group: &str) -> Option<(String, axum::Router)> {
    let state = build_state(leader, group).await?;
    let router = musubi_server::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    Some((format!("127.0.0.1:{}", addr.port()), router))
}

async fn build_state(leader: bool, group: &str) -> Option<AppState> {
    build_state_in(leader, group, "local").await
}

/// A node of `group` in `region` (multi-region placement tests).
async fn build_state_in(leader: bool, group: &str, region: &str) -> Option<AppState> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    // a node runs world ticks, snapshots, journals, match loops and failover rounds on this
    // pool at once; 3 starved them into 30 s acquire timeouts on the 4-thread CI runner
    cfg.database.max_connections = 5;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let hub = Arc::new(Hub::new());
    let runtime = test_runtime();
    let cluster = test_cluster_in(&db, group, region).await;
    let matchmaker = Arc::new(musubi_matchmaker::Matchmaker::default());
    let parties = Arc::new(musubi_server::realtime::PartyRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
    let relayed = Arc::new(musubi_server::realtime::RelayedRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
    let fanout = musubi_server::fanout::Fanout::new(
        hub.clone(),
        cluster.clone(),
        matchmaker.clone(),
        parties.clone(),
        relayed.clone(),
    );
    fanout.set_leader(leader);
    let fleets = Arc::new(musubi_fleet::FleetService::new(
        db.clone(),
        cluster.as_ref().unwrap().node_id().0,
        "http://127.0.0.1:0",
        "defaultkey",
    ));
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(musubi_server::realtime::WorldRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
        Some(db.clone()),
    ));
    worlds.set_region(region);
    fanout.set_worlds(worlds.clone());
    worlds.clone().spawn_ticker(fanout.clone());
    let logins = Arc::new(musubi_server::logins::Social::new(cfg.social.clone()));
    let schedules = Arc::new(musubi_server::schedules::Schedules::from_config(
        &cfg.runtime,
    ));
    Some(AppState {
        auth: AuthService::new(db.clone(), &cfg.session).unwrap(),
        storage: StorageService::new(db.clone()),
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            fanout.clone(),
        ),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        economy: musubi_economy::EconomyService::new(db.clone()),
        matchmaker,
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: worlds.clone(),
        liveops: None,
        voice: None,
        rtc: None,
        apple_iap: None,
        google_iap: None,
        steam_iap: None,
        runtime: runtime.clone(),
        schedules,
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        cfg: Arc::new(cfg),
        events: Arc::new(musubi_server::events::Events::new(db.clone())),
        banned: Arc::new(musubi_server::bans::Bans::default()),
        keys: Arc::new(musubi_server::keys::ServerKeys::default()),
        logins,
        draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        db,
        started_at: std::time::Instant::now(),
        metrics: musubi_server::metrics::install(),
        limits: Arc::new(musubi_server::limits::RateLimiter::new(
            &musubi_core::config::LimitsConfig {
                enabled: false,
                ..Default::default()
            },
        )),
    })
}

async fn token(router: &axum::Router) -> (String, String) {
    let body = json!({"id": format!("ws-{}", uuid::Uuid::now_v7().simple())}).to_string();
    let req = Request::post("/v1/auth/device")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (
        v["access_token"]
            .as_str()
            .unwrap_or_else(|| panic!("device auth failed: {status} {v}"))
            .into(),
        v["account_id"].as_str().unwrap().into(),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &str, token: &str, format: &str) -> Ws {
    let (ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}&format={format}"))
            .await
            .unwrap();
    ws
}

async fn send(ws: &mut Ws, format: &str, cid: Option<&str>, msg: ClientMessage) {
    let env = Envelope {
        cid: cid.map(Into::into),
        msg,
    };
    let m = if format == "json" {
        Message::Text(serde_json::to_string(&env).unwrap())
    } else {
        Message::Binary(Encoding::Postcard.encode(&env).unwrap().to_vec())
    };
    ws.send(m).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Envelope<ServerMessage> {
    let m = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timeout")
        .unwrap()
        .unwrap();
    match m {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        Message::Binary(b) => Encoding::Postcard.decode(&b).unwrap(),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn ping_follow_status_and_disconnect_in_both_encodings() {
    let Some((addr, router)) = start().await else {
        return;
    };
    for format in ["json", "binary"] {
        let (ta, acc_a) = token(&router).await;
        let (tb, _) = token(&router).await;

        let mut b = connect(&addr, &tb, format).await;
        send(&mut b, format, Some("p1"), ClientMessage::Ping).await;
        let r = recv(&mut b).await;
        assert_eq!(r.cid.as_deref(), Some("p1"));
        assert_eq!(r.msg, ServerMessage::Pong);

        let acc_a_id = acc_a.parse().unwrap();
        send(
            &mut b,
            format,
            Some("f"),
            ClientMessage::StatusFollow {
                account_ids: vec![acc_a_id],
            },
        )
        .await;
        assert!(
            matches!(recv(&mut b).await.msg, ServerMessage::Status { presences } if presences.is_empty())
        );

        let mut a = connect(&addr, &ta, format).await;
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins.len(), 1);
                assert_eq!(joins[0].account_id, acc_a_id);
                assert!(leaves.is_empty());
            }
            m => panic!("{m:?}"),
        }

        send(
            &mut a,
            format,
            None,
            ClientMessage::StatusUpdate {
                status: Some("in menu".into()),
            },
        )
        .await;
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins[0].status.as_deref(), Some("in menu"));
                assert_eq!(leaves[0].status, None);
            }
            m => panic!("{m:?}"),
        }

        // garbage frame → error, connection stays up
        a.send(Message::Text("not json".into())).await.unwrap();
        assert!(matches!(
            recv(&mut a).await.msg,
            ServerMessage::Error { .. }
        ));
        send(&mut a, format, Some("p2"), ClientMessage::Ping).await;
        assert_eq!(recv(&mut a).await.msg, ServerMessage::Pong);

        a.close(None).await.unwrap();
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert!(joins.is_empty());
                assert_eq!(leaves.len(), 1);
            }
            m => panic!("{m:?}"),
        }
    }
}

#[tokio::test]
async fn rejects_bad_token() {
    let Some((addr, _)) = start().await else {
        return;
    };
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token=garbage"))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("401"), "{err}");
}

/// Tests in one binary run concurrently against a shared database: apply migrations once per process.
async fn migrate_once(db: &sqlx::PgPool) {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async { musubi_server::db::migrate(db).await.unwrap() })
        .await;
}

#[tokio::test]
async fn chat_room_and_direct_with_history_and_blocks() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("lobby-{}", uuid::Uuid::now_v7().simple());

    // A joins empty room, B joins → A gets presence event, B sees A
    send(
        &mut a,
        f,
        Some("j"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let r = recv(&mut a).await;
    let room_id = match r.msg {
        ServerMessage::Channel { id, presences, .. } => {
            assert!(presences.is_empty());
            id
        }
        m => panic!("{m:?}"),
    };
    assert_eq!(room_id, format!("2.{room}"));
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Channel { presences, .. } if presences.len() == 1)
    );
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::ChannelPresenceEvent { joins, .. } if joins.len() == 1)
    );

    // room message: non-persistent, both receive, sender also gets ack
    send(
        &mut a,
        f,
        Some("m1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: serde_json::json!({"text": "hi"}),
        },
    )
    .await;
    let mut got_ack = false;
    let mut got_msg = false;
    for _ in 0..2 {
        let r = recv(&mut a).await;
        match r.msg {
            ServerMessage::ChannelMessageAck { persistent, .. } => {
                assert_eq!(r.cid.as_deref(), Some("m1"));
                assert!(!persistent);
                got_ack = true;
            }
            ServerMessage::ChannelMessage(m) => {
                assert_eq!(m.content["text"], "hi");
                got_msg = true;
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(got_ack && got_msg);
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelMessage(m) if m.sender_id == Some(acc_a.parse().unwrap()))
    );

    // sending to a channel you haven't joined is denied
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: "2.other".into(),
            content: serde_json::json!({}),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));

    // direct chat: persisted, editable, removable, visible via HTTP history
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    let dm = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: dm.clone(),
            content: serde_json::json!({"text": "secret"}),
        },
    )
    .await;
    let mut mid = None;
    for _ in 0..2 {
        if let ServerMessage::ChannelMessageAck {
            message_id,
            persistent,
            ..
        } = recv(&mut a).await.msg
        {
            assert!(persistent);
            mid = Some(message_id);
        }
    }
    let mid = mid.unwrap();
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageUpdate {
            channel_id: dm.clone(),
            message_id: mid.clone(),
            content: serde_json::json!({"text": "edited"}),
        },
    )
    .await;
    for _ in 0..2 {
        match recv(&mut a).await.msg {
            ServerMessage::ChannelMessageAck { code, .. } => assert_eq!(code, 1),
            ServerMessage::ChannelMessage(m) => assert_eq!(m.content["text"], "edited"),
            m => panic!("{m:?}"),
        }
    }
    // B is not in the DM channel yet but can read history over HTTP
    let req = Request::get(format!("/v1/channel/{dm}/messages"))
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["messages"][0]["content"]["text"], "edited");
    assert_eq!(v["messages"][0]["code"], 1);

    // B blocks A → A can no longer join the DM; a stranger cannot read it either
    let req = Request::post("/v1/friends/block")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"ids": [acc_a]}).to_string()))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelLeave {
            channel_id: dm.clone(),
        },
    )
    .await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    let (tc, _) = token(&router).await;
    let req = Request::get(format!("/v1/channel/{dm}/messages"))
        .header(header::AUTHORIZATION, format!("Bearer {tc}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // A disconnects → B gets a room leave event
    a.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn matchmaker_pairs_two_players_and_remove_works() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = |rank: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region} +rank:>=5"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: [("rank".to_string(), rank)].into(),
        count_multiple: 1,
    };
    // C has too low a rank for A's query → never matched with A
    send(&mut c, f, None, add(1.0)).await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut a, f, Some("t"), add(10.0)).await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("t"));
    let ticket_a = match r.msg {
        ServerMessage::MatchmakerTicket { ticket } => ticket,
        m => panic!("{m:?}"),
    };
    // invalid counts → error
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchmakerAdd {
            min_count: 1,
            max_count: 1,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error { .. }
    ));

    send(&mut b, f, None, add(7.0)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let ma = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched {
            ticket,
            token,
            users,
            me,
            ..
        } => {
            assert_eq!(ticket, ticket_a);
            assert_eq!(users.len(), 2);
            assert_eq!(me.numeric_properties["rank"], 10.0);
            token
        }
        m => panic!("{m:?}"),
    };
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(token, ma);
            assert_eq!(users.len(), 2);
        }
        m => panic!("{m:?}"),
    }
    // consumed ticket cannot be removed; C's own can
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchmakerRemove { ticket: ticket_a },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchmakerRemove {
            ticket: "nope".into(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn parties_create_join_requests_leader_data_and_party_matchmaking() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;

    // A creates a closed party of 2
    send(
        &mut a,
        f,
        Some("c"),
        ClientMessage::PartyCreate {
            open: false,
            max_size: 2,
        },
    )
    .await;
    let (pid, pa) = match recv(&mut a).await.msg {
        ServerMessage::Party {
            party_id,
            me,
            leader,
            presences,
            open,
            ..
        } => {
            assert!(!open);
            assert_eq!(leader, me);
            assert_eq!(presences.len(), 1);
            (party_id, me)
        }
        m => panic!("{m:?}"),
    };
    // B asks to join → A gets a join request, B gets nothing yet
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    let pb = match recv(&mut a).await.msg {
        ServerMessage::PartyJoinRequest { presences, .. } => presences[0].clone(),
        m => panic!("{m:?}"),
    };
    // C is not leader → cannot accept
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { joins, .. } if joins.len() == 1)
    );
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Party { presences, .. } if presences.len() == 2)
    );
    // full now: C's request is queued but cannot be accepted
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    let pc = match recv(&mut a).await.msg {
        ServerMessage::PartyJoinRequest { presences, .. } => presences[0].clone(),
        m => panic!("{m:?}"),
    };
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pc.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyRemove {
            party_id: pid.clone(),
            presence: pc,
        },
    )
    .await;

    // party data reaches both members
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyDataSend {
            party_id: pid.clone(),
            op_code: 7,
            data: vec![1, 2, 3],
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::PartyData {
                op_code,
                data,
                presence,
                ..
            } => {
                assert_eq!(op_code, 7);
                assert_eq!(data, vec![1, 2, 3]);
                assert_eq!(presence.session_id, pb.session_id);
            }
            m => panic!("{m:?}"),
        }
    }

    // party matchmaking: only the leader may; both get the ticket
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyMatchmakerAdd {
            party_id: pid.clone(),
            min_count: 2,
            max_count: 2,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    let region = uuid::Uuid::now_v7().simple().to_string();
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyMatchmakerAdd {
            party_id: pid.clone(),
            min_count: 2,
            max_count: 2,
            query: format!("+region:{region}"),
            string_properties: [("region".to_string(), region.clone())].into(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    let ticket = match recv(&mut a).await.msg {
        ServerMessage::PartyMatchmakerTicket { ticket, .. } => ticket,
        m => panic!("{m:?}"),
    };
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::PartyMatchmakerTicket { ticket: t, .. } if t == ticket)
    );
    // the party alone fills a 2-player match
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchmakerMatched {
                ticket: t, users, ..
            } => {
                assert_eq!(t, ticket);
                assert_eq!(users.len(), 2);
            }
            m => panic!("{m:?}"),
        }
    }

    // leader promotes B, then leaves → B stays leader, A can create a new party
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyPromote {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyLeader { presence, .. } if presence.session_id == pb.session_id)
    );
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::PartyLeader { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyLeave {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::PartyPresenceEvent { leaves, .. } if leaves[0].session_id == pa.session_id)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Party { .. }
    ));
    // B closes its party; A's open party accepts C directly
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyClose {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::PartyClose { .. }
    ));
    send(&mut c, f, None, ClientMessage::PartyJoin { party_id: pid }).await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn relayed_matches_create_join_data_and_matchmaker_token() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;

    send(
        &mut a,
        f,
        Some("m"),
        ClientMessage::MatchCreate {
            name: Some("duel".into()),
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id,
            authoritative,
            label,
            size,
            ..
        } => {
            assert!(!authoritative);
            assert_eq!(label.as_deref(), Some("duel"));
            assert_eq!(size, 1);
            match_id
        }
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { size: 2, .. }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );

    // data goes to the other member only
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 1,
            data: vec![9],
            presences: vec![],
            reliable: true,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::MatchData {
            op_code,
            data,
            presence,
            ..
        } => {
            assert_eq!(op_code, 1);
            assert_eq!(data, vec![9]);
            assert!(presence.is_some());
        }
        m => panic!("{m:?}"),
    }
    // outsider cannot send
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 1,
            data: vec![],
            presences: vec![],
            reliable: true,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));

    // leave: B notified; A leaving empties and removes the match
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));

    // matchmaker token → both land in the same relayed match
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region}"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: Default::default(),
        count_multiple: 1,
    };
    send(&mut a, f, None, add.clone()).await;
    recv(&mut a).await;
    send(&mut b, f, None, add).await;
    recv(&mut b).await;
    let tok = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { token, .. } => token,
        m => panic!("{m:?}"),
    };
    recv(&mut b).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some(tok.clone()),
            metadata: Default::default(),
        },
    )
    .await;
    let m1 = match recv(&mut a).await.msg {
        ServerMessage::Match { match_id, .. } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some(tok),
            metadata: Default::default(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Match { match_id, size: 2, .. } if match_id == m1)
    );
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some("bogus".into()),
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

fn test_runtime() -> Arc<musubi_runtime::Runtime> {
    let rt = Arc::new(musubi_runtime::Runtime::new(musubi_runtime::Limits::default()).unwrap());
    rt.load_bytes("example", musubi_runtime::EXAMPLE_WAT.as_bytes())
        .unwrap();
    rt
}

#[tokio::test]
async fn rpc_over_socket() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        Some("r1"),
        ClientMessage::Rpc {
            id: "whoami".into(),
            payload: json!({}),
        },
    )
    .await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("r1"));
    match r.msg {
        ServerMessage::Rpc { id, payload } => {
            assert_eq!(id, "whoami");
            assert_eq!(payload["account_id"], acc);
            assert!(payload["session_id"].is_string());
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        "json",
        None,
        ClientMessage::Rpc {
            id: "spin".into(),
            payload: json!({}),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));
}

async fn test_cluster_in(
    db: &sqlx::PgPool,
    group: &str,
    region: &str,
) -> Option<Arc<musubi_cluster::Cluster>> {
    let c = Arc::new(
        musubi_cluster::Cluster::new(
            db.clone(),
            "test-node",
            region,
            vec!["all".into()],
            "127.0.0.1:0",
        )
        .with_group(group),
    );
    c.start().await.unwrap();
    Some(c)
}

/// Two independent "nodes" (separate hubs, same database): a chat message sent on node 1 reaches a
/// session on node 2, and so does a notification.
#[tokio::test]
async fn cross_node_chat_and_notification_fanout() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _router2)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    let room = format!("x-{}", uuid::Uuid::now_v7().simple());
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let rid = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room,
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    recv(&mut b).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: rid,
            content: json!({"text": "across"}),
        },
    )
    .await;
    // A gets ack + own message; B (other node) gets the message through the bus
    recv(&mut a).await;
    recv(&mut a).await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelMessage(m) if m.content["text"] == "across")
    );

    // friend request from B (via node 1's HTTP) → live notification to A on node 1 and to B's session on node 2 after accept
    let req = Request::post("/v1/friends")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"ids": [acc_a]}).to_string()))
        .unwrap();
    assert_eq!(router1.clone().oneshot(req).await.unwrap().status(), 200);
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Notification { code: -2, .. }
    ));
    let (_, acc_b) = (0, b);
    let _ = acc_b;
}

#[tokio::test]
async fn authoritative_echo_match_lifecycle() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    // server creates a match from the built-in `echo` module
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "max_players": 2, "label": "duel", "empty_ticks": 2}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    assert_eq!(info["label"], "duel");

    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 2,
            ..
        }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );
    // full
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));

    // input is relayed by the handler on the next tick, to everyone, as a server message
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 5,
            data: vec![1, 2],
            presences: vec![],
            reliable: true,
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchData {
                op_code,
                data,
                presence,
                ..
            } => {
                assert_eq!(op_code, 5);
                assert_eq!(data, vec![1, 2]);
                assert!(presence.is_none());
            }
            m => panic!("{m:?}"),
        }
    }
    // listing shows it with size 2
    let req = Request::get("/v1/matches?label=duel")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["match_id"] == mid && m["size"] == 2));

    // leave → presence event; when empty for 2 ticks the match ends and disappears
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let req = Request::get(format!("/v1/server/matches/{mid}"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 404);
}

#[tokio::test]
async fn match_snapshots_persist_and_resume_on_another_registry() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "label": "persist", "empty_ticks": 1000}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    recv(&mut a).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let row: Option<(String, i64)> =
        sqlx::query_as("select label, tick from musubi.match_snapshots where match_id = $1")
            .bind(uuid::Uuid::parse_str(&mid).unwrap())
            .fetch_optional(&db)
            .await
            .unwrap();
    let (label, _) = row.expect("snapshot written on join");
    assert_eq!(label, "persist");

    // another node: detach the row from its owner and resume it there
    sqlx::query("update musubi.match_snapshots set node_id = null where match_id = $1")
        .bind(uuid::Uuid::parse_str(&mid).unwrap())
        .execute(&db)
        .await
        .unwrap();
    let other = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    let hub2 = Arc::new(Hub::new());
    let resumed = other.recover(hub2, &[]).await.unwrap();
    assert_eq!(resumed, vec![mid.clone()]);
    let resumed_info = other.get(&mid).unwrap().info().await;
    assert_eq!(resumed_info.label, "persist");
    assert!(resumed_info.tick > 0, "resumes from the snapshot tick");
    other.get(&mid).unwrap().terminate(0);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(other.get(&mid).is_none());
}

#[tokio::test]
async fn cross_node_presence_follow() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut b,
        f,
        None,
        ClientMessage::StatusFollow {
            account_ids: vec![acc_a.parse().unwrap()],
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Status { presences } if presences.is_empty())
    );
    let mut a = connect(&addr1, &ta, f).await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { joins, .. } if joins.len() == 1 && joins[0].account_id.to_string() == acc_a)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::StatusUpdate {
            status: Some("remote".into()),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { joins, .. } if joins[0].status.as_deref() == Some("remote"))
    );
    a.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

fn test_matches(
    db: &sqlx::PgPool,
    runtime: &Arc<musubi_runtime::Runtime>,
) -> Arc<musubi_server::realtime::MatchRegistry> {
    let m = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    assert_eq!(m.register_wasm(runtime), vec!["demo"]);
    Arc::new(m)
}

#[tokio::test]
async fn wasm_match_module_runs_as_authoritative_match() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": "demo", "params": {}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(info["label"], "demo");
    assert_eq!(info["tick_rate"], 10);
    let mid = info["match_id"].as_str().unwrap().to_string();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let req = Request::get(format!("/v1/server/matches/{mid}"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["tick"].as_u64().unwrap() >= 1, "{v}");
}

#[tokio::test]
async fn cross_node_matchmaking() {
    let group = test_group();
    let Some((addr1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((addr2, _)) = start_with(false, &group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    let mode = format!("xn-{}", uuid::Uuid::now_v7().simple());
    let add = |mode: &str| ClientMessage::MatchmakerAdd {
        query: format!("+properties.mode:{mode}"),
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
        string_properties: [("mode".to_string(), mode.to_string())]
            .into_iter()
            .collect(),
        numeric_properties: Default::default(),
    };
    // follower node adds first: its ticket is replicated to the leader
    send(&mut b, f, None, add(&mode)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    send(&mut a, f, None, add(&mode)).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let ta_tok = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(users.len(), 2);
            token
        }
        m => panic!("{m:?}"),
    };
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(token, ta_tok);
            assert_eq!(users.len(), 2);
        }
        m => panic!("{m:?}"),
    }
}

#[tokio::test]
async fn quic_transport_ping_and_rpc() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr =
        musubi_server::realtime::quic::start_with(state, "127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap();
    let (ta, acc) = token(&router).await;

    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |cid: &str, msg: ClientMessage| {
        Frame::Text(
            serde_json::to_string(&Envelope {
                cid: Some(cid.into()),
                msg,
            })
            .unwrap(),
        )
    };
    Q::write_frame(&mut tx, env("1", ClientMessage::Ping))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong));
    Q::write_frame(
        &mut tx,
        env(
            "2",
            ClientMessage::Rpc {
                id: "whoami".into(),
                payload: json!({}),
            },
        ),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Rpc { payload, .. } if payload["account_id"] == acc));

    // bad token is rejected with an error frame
    let conn2 = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap();
    Q::write_frame(&mut tx2, Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx2, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}

#[tokio::test]
async fn webtransport_ping_and_rpc() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, hash) =
        musubi_transport::webtransport::dev_self_signed("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = musubi_server::realtime::webtransport::start_with(state, cfg)
        .await
        .unwrap();
    let (ta, acc) = token(&router).await;

    let ccfg = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([hash.clone()])
        .build();
    let conn = wtransport::Endpoint::client(ccfg)
        .unwrap()
        .connect(format!("https://127.0.0.1:{}", addr.port()))
        .await
        .unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap().await.unwrap();
    use musubi_transport::framing as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |cid: &str, msg: ClientMessage| {
        Frame::Text(
            serde_json::to_string(&Envelope {
                cid: Some(cid.into()),
                msg,
            })
            .unwrap(),
        )
    };
    Q::write_frame(&mut tx, env("1", ClientMessage::Ping))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong));
    Q::write_frame(
        &mut tx,
        env(
            "2",
            ClientMessage::Rpc {
                id: "whoami".into(),
                payload: json!({}),
            },
        ),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Rpc { payload, .. } if payload["account_id"] == acc));

    // bad token is rejected with an error frame
    let conn2 = wtransport::Endpoint::client(
        wtransport::ClientConfig::builder()
            .with_bind_default()
            .with_server_certificate_hashes([hash])
            .build(),
    )
    .unwrap()
    .connect(format!("https://127.0.0.1:{}", addr.port()))
    .await
    .unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap().await.unwrap();
    Q::write_frame(&mut tx2, Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx2, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}

#[tokio::test]
async fn cross_node_party() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, acc_b) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
        },
    )
    .await;
    let pid = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    // B lives on node 2; the party is homed on node 1
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Party { presences, .. } if presences.len() == 2)
    );
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { joins, .. } if joins[0].account_id.to_string() == acc_b)
    );
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyDataSend {
            party_id: pid.clone(),
            op_code: 7,
            data: vec![9],
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        assert!(
            matches!(recv(ws).await.msg, ServerMessage::PartyData { op_code: 7, data, .. } if data == vec![9])
        );
    }
    // leader-only op from a non-leader on the remote node is refused with an async error
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyClose {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    // remote disconnect removes B from the party on the home node
    b.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn quic_unreliable_match_data_uses_datagrams() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr =
        musubi_server::realtime::quic::start_with(state, "127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap();
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": "echo", "params": {"tick_rate": 30, "empty_ticks": 1000}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();

    let (ta, _) = token(&router).await;
    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |msg: ClientMessage| {
        Frame::Text(serde_json::to_string(&Envelope { cid: None, msg }).unwrap())
    };
    Q::write_frame(
        &mut tx,
        env(ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        }),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    assert!(matches!(
        serde_json::from_str::<Envelope<ServerMessage>>(&t)
            .unwrap()
            .msg,
        ServerMessage::Match { .. }
    ));
    // client may also send on a datagram; the echo match relays it unreliably → arrives as a datagram
    conn.send_datagram(Q::encode_datagram(&env(ClientMessage::MatchDataSend {
        match_id: mid.clone(),
        op_code: 3,
        data: vec![4, 2],
        presences: vec![],
        reliable: false,
    })))
    .unwrap();
    let d = tokio::time::timeout(std::time::Duration::from_secs(3), conn.read_datagram())
        .await
        .unwrap()
        .unwrap();
    let Frame::Text(t) = Q::decode_datagram(d).unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(
        matches!(r.msg, ServerMessage::MatchData { op_code: 3, reliable: false, data, .. } if data == vec![4, 2])
    );
}

#[tokio::test]
async fn cross_node_relayed_match() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchCreate {
            name: Some("xn".into()),
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id,
            authoritative: false,
            ..
        } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { size: 2, .. }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 11,
            data: vec![5],
            presences: vec![],
            reliable: true,
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchData { op_code: 11, data, .. } if data == vec![5])
    );
    b.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn matchmaker_allocates_a_dedicated_server_when_tickets_ask_for_a_fleet() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let name = format!("mmfleet-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/fleets")
        .header("x-musubi-server-key", k)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": name, "buffer": 1, "max_servers": 1, "port_start": 48000, "port_end": 48100, "spec": {"command": ["sleep", "60"]}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let fleet: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let fid = fleet["id"].as_str().unwrap().to_string();
    state.fleets.reconcile().await.unwrap();
    let servers = state
        .fleets
        .servers(uuid::Uuid::parse_str(&fid).unwrap(), None)
        .await
        .unwrap();
    let sid = servers[0].id;
    state.fleets.sdk_ready(sid).await.unwrap();

    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let add = || ClientMessage::MatchmakerAdd {
        query: "+properties.fleet:".to_string() + &name,
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
        string_properties: [("fleet".to_string(), name.clone())].into_iter().collect(),
        numeric_properties: Default::default(),
    };
    send(&mut a, f, None, add()).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut b, f, None, add()).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    // every test state is a matchmaker leader on the shared Postgres bus, so another leader may
    // deliver the same group first (without a server); wait for the delivery that carries ours
    for ws in [&mut a, &mut b] {
        let mut ok = false;
        for _ in 0..4 {
            match recv(ws).await.msg {
                ServerMessage::MatchmakerMatched {
                    server: Some(srv), ..
                } => {
                    assert_eq!(srv.server_id, sid.to_string());
                    assert_eq!(srv.port, 48000);
                    assert!(!srv.connect_token.is_empty());
                    ok = true;
                    break;
                }
                ServerMessage::MatchmakerMatched { server: None, .. } => continue,
                m => panic!("{m:?}"),
            }
        }
        assert!(
            ok,
            "no matched delivery with a server; servers: {:?}",
            state
                .fleets
                .servers(uuid::Uuid::parse_str(&fid).unwrap(), None)
                .await
                .unwrap()
        );
    }
    let gs = state.fleets.server(sid).await.unwrap();
    assert_eq!(gs.state, "allocated");
    state
        .fleets
        .delete(uuid::Uuid::parse_str(&fid).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn world_meshing_sim_api_and_client_deltas() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, _) = post("/v1/server/worlds".into(), json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0, "lease_ttl_ticks": 600, "handover_timeout_ticks": 30}})).await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s1, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s2, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 409, "cell already leased");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s2, "cells": [{"x": 1, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);
    // s2 cannot spawn in s1's cell
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": s2, "x": 50.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403);

    // client subscribes before anything exists
    let f = "json";
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;

    let (st, e) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": s1, "x": 50.0, "y": 50.0, "components": {"hp": 100}}),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { spawned, .. } => {
            assert_eq!(spawned.len(), 1);
            assert_eq!(spawned[0].id, eid);
            assert_eq!(spawned[0].components[0].name, "hp");
        }
        m => panic!("{m:?}"),
    }
    // authoritative update → delta with the changed component only
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 60.0, "y": 50.0, "components": {"hp": 90}}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { updated, .. } => {
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].x, 60.0);
            assert_eq!(updated[0].components.len(), 1);
            assert_eq!(updated[0].components[0].version, 2);
        }
        m => panic!("{m:?}"),
    }
    // player input is routed to the owner's event queue
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldInput {
            world: world.clone(),
            entity: eid.to_string(),
            input: json!({"move": [1, 0]}),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (_, ev) = post(
        format!("/v1/server/worlds/{world}/events?sim={s1}"),
        json!({}),
    )
    .await;
    let _ = ev;
    let req = Request::get(format!("/v1/server/worlds/{world}/events?sim={s1}"))
        .header("x-musubi-server-key", k)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let ev: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "input" && e["entity"] == eid.to_string()),
        "{ev}"
    );
    // ghost: entity near the border is visible to s2's view of cell (1,0)
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 95.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    let req = Request::get(format!("/v1/server/worlds/{world}/view/1/0"))
        .header("x-musubi-server-key", k)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["ghosts"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["owned"].as_array().unwrap().len(), 0);
    // handover: cross the border past hysteresis → s2 gets a request, acks, owns it
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 110.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    let mut got = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let req = Request::get(format!("/v1/server/worlds/{world}/events?sim={s2}"))
            .header("x-musubi-server-key", k)
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let ev: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        if ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == eid.to_string())
        {
            got = true;
            break;
        }
    }
    assert!(got, "s2 never got the handover request");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": eid, "x": 111.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403, "no authority before ack");
    let (st, a) = post(
        format!("/v1/server/worlds/{world}/handover/ack"),
        json!({"sim": s2, "entity": eid}),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": eid, "x": 120.0, "y": 50.0, "components": {"hp": 80}}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 1.0, "y": 1.0}),
    )
    .await;
    assert_eq!(st, 403, "old owner lost authority");
    // the client saw only deltas, never the ownership change
    let mut saw_update = false;
    for _ in 0..10 {
        match recv(&mut c).await.msg {
            ServerMessage::WorldDelta {
                updated, despawned, ..
            } => {
                if updated.iter().any(|u| u.id == eid && u.x == 120.0) {
                    saw_update = true;
                    break;
                }
                if despawned.contains(&eid) {
                    break;
                }
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(saw_update);
}

#[tokio::test]
async fn shard_controller_assigns_cells_and_recovers_orphans() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let world = format!("sc{}", uuid::Uuid::now_v7().simple());
    let call = |method: &'static str, path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    // leases last 15 ticks (0.5 s); the controller runs every 30 ticks
    let (st, _) = call("POST", "/v1/server/worlds".into(), json!({"name": world, "config": {"cell_size": 100.0, "lease_ttl_ticks": 15}, "controller": {"sim_timeout_ticks": 15}})).await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/sims"),
        json!({"sim": s1, "capacity": 4}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/sims"),
        json!({"sim": s2, "capacity": 4}),
    )
    .await;
    assert_eq!(st, 200);
    // a client looks at an unowned cell → the controller leases it to a simulator
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, "json").await;
    // simulators heartbeat (auth above may have taken longer than sim_timeout_ticks)
    for sim in [s1, s2] {
        call(
            "POST",
            format!("/v1/server/worlds/{world}/renew"),
            json!({"sim": sim}),
        )
        .await;
    }
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 50.0,
            y: 50.0,
            radius: 10.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    let cells = info["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1, "{info}");
    let owner = uuid::Uuid::parse_str(cells[0][1].as_str().unwrap()).unwrap();
    assert!(owner == s1 || owner == s2);
    let (_, ev) = call(
        "GET",
        format!("/v1/server/worlds/{world}/events?sim={owner}"),
        json!({}),
    )
    .await;
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_assigned" && e["reason"] == "interest"),
        "{ev}"
    );
    // a simulator spawning into a free cell gets that cell
    let other = if owner == s1 { s2 } else { s1 };
    call(
        "POST",
        format!("/v1/server/worlds/{world}/renew"),
        json!({"sim": other}),
    )
    .await;
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": other, "x": 550.0, "y": 50.0}),
    )
    .await;
    let (_, dbg) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    assert_eq!(st, 200, "{dbg}");
    let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    assert_eq!(info["cells"].as_array().unwrap().len(), 2, "{info}");
    // `owner` dies (never renews); `other` keeps renewing → the orphaned cell moves to `other`
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut moved = false;
    while tokio::time::Instant::now() < deadline {
        call(
            "POST",
            format!("/v1/server/worlds/{world}/renew"),
            json!({"sim": other}),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
        let cells = info["cells"].as_array().unwrap();
        if cells.len() == 2
            && cells
                .iter()
                .all(|c| c[1].as_str().unwrap() == other.to_string())
        {
            moved = true;
            break;
        }
    }
    assert!(moved, "orphaned cell was not re-leased");
    let (_, ev) = call(
        "GET",
        format!("/v1/server/worlds/{world}/events?sim={other}"),
        json!({}),
    )
    .await;
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_assigned" && e["from"] == owner.to_string()),
        "{ev}"
    );
}

/// Moves every owned entity +10 on x each tick: it will cross into the neighbour's cell.
struct Mover;
impl musubi_sim_sdk::Simulator for Mover {
    fn tick(
        &mut self,
        _cell: musubi_mesh::CellId,
        view: &musubi_sim_sdk::View,
    ) -> Vec<musubi_sim_sdk::Update> {
        view.owned
            .iter()
            .map(|e| musubi_sim_sdk::Update {
                entity: e.id.to_string(),
                pos: Some((e.x + 10.0, e.y)),
                components: Default::default(),
            })
            .collect()
    }
}

#[tokio::test]
async fn sim_sdk_two_simulators_hand_an_entity_over() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let world = format!("sdk{}", uuid::Uuid::now_v7().simple());
    let client = musubi_sim_sdk::WorldClient::new(&base, "defaultkey", &world);
    client
        .create_world(json!({"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0, "handover_timeout_ticks": 60, "lease_ttl_ticks": 600}), json!({}))
        .await
        .unwrap();
    let (s1, s2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    client.register(s1, 1).await.unwrap();
    client.register(s2, 1).await.unwrap();
    client
        .lease(s1, &[musubi_mesh::CellId { x: 0, y: 0 }])
        .await
        .unwrap();
    client
        .lease(s2, &[musubi_mesh::CellId { x: 1, y: 0 }])
        .await
        .unwrap();
    let e = client
        .spawn(s1, 50.0, 50.0, json!({"kind": "car"}), false)
        .await
        .unwrap();
    let (tx1, rx1) = tokio::sync::oneshot::channel::<()>();
    let (tx2, rx2) = tokio::sync::oneshot::channel::<()>();
    let cfg = || musubi_sim_sdk::SimConfig {
        capacity: 1,
        hz: 30,
        renew_every: 5,
    };
    let c1 = client.clone();
    let c2 = client.clone();
    let h1 = tokio::spawn(async move {
        musubi_sim_sdk::run_sim(c1, s1, cfg(), Mover, async {
            let _ = rx1.await;
        })
        .await
    });
    let h2 = tokio::spawn(async move {
        musubi_sim_sdk::run_sim(c2, s2, cfg(), Mover, async {
            let _ = rx2.await;
        })
        .await
    });
    // the car drives from x=50 into cell (1,0); s2 must end up owning and moving it
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut handed_over = false;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let v = client
            .view(musubi_mesh::CellId { x: 1, y: 0 })
            .await
            .unwrap();
        if let Some(car) = v.owned.iter().find(|c| c.id == e.id) {
            if car.x > 130.0 {
                handed_over = true;
                break;
            }
        }
    }
    let _ = tx1.send(());
    let _ = tx2.send(());
    h1.await.unwrap().unwrap();
    h2.await.unwrap().unwrap();
    assert!(
        handed_over,
        "entity never continued moving under s2: {:?}",
        client.info().await.unwrap()
    );
}

#[tokio::test]
async fn cross_node_world_subscription_and_sim_redirect() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, router2)) = start_in(&group).await else {
        return;
    };
    let base1 = format!("http://{addr1}");
    let world = format!("xw{}", uuid::Uuid::now_v7().simple());
    let c1 = musubi_sim_sdk::WorldClient::new(&base1, "defaultkey", &world);
    c1.create_world(json!({"cell_size": 100.0}), json!({}))
        .await
        .unwrap();
    let sim = uuid::Uuid::now_v7();
    c1.register(sim, 100).await.unwrap();
    // simulator talking to the wrong node is told where the world lives (no advertise in tests → 409)
    let c2 = musubi_sim_sdk::WorldClient::new(&format!("http://{addr2}"), "defaultkey", &world);
    match c2.register(sim, 100).await {
        Err(musubi_sim_sdk::SdkError::Server { status: 409, body }) => {
            assert!(body.contains("lives on node"), "{body}")
        }
        r => panic!("{r:?}"),
    }
    let _ = router2;
    // a client on node 2 subscribes to the world homed on node 1 and receives deltas
    let (tc, _) = token(&router1).await;
    let mut c = connect(&addr2, &tc, "json").await;
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 500.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let e = c1
        .spawn(sim, 50.0, 50.0, json!({"hp": 1}), false)
        .await
        .unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { spawned, .. } => assert_eq!(spawned[0].id, e.id),
        m => panic!("{m:?}"),
    }
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldInput {
            world: world.clone(),
            entity: e.id.to_string(),
            input: json!({"fire": true}),
        },
    )
    .await;
    let mut got = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if c1
            .events(sim)
            .await
            .unwrap()
            .iter()
            .any(|ev| ev["type"] == "input" && ev["entity"] == e.id.to_string())
        {
            got = true;
            break;
        }
    }
    assert!(
        got,
        "input from node 2 never reached the simulator on node 1"
    );
    // unknown world → error reply
    send(
        &mut c,
        "json",
        Some("q"),
        ClientMessage::WorldSubscribe {
            world: "nope".into(),
            x: 0.0,
            y: 0.0,
            radius: 1.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// Fake Zanshin + Aurix: records events, answers flags and tokens.
async fn mock_upstreams() -> (String, Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::routing::{get, post};
    use axum::Json;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let s2 = seen.clone();
    let app = axum::Router::new()
        .route("/server/v1/events", post(move |Json(b): Json<Value>| {
            let s2 = s2.clone();
            async move {
                let n = b["events"].as_array().map(|a| a.len()).unwrap_or(0);
                s2.lock().unwrap().push(b);
                Json(json!({"accepted": n, "rejected": 0}))
            }
        }))
        .route("/server/v1/flags", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            Json(json!({"flags": [{"key": "new_map", "value": true, "external_id": q.get("external_id")}]}))
        }))
        .route("/v1/users", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            Json(json!([{"id": "u1", "external_id": q.get("q")}]))
        }))
        .route("/v1/moderation/ban", post({
            let s3 = seen.clone();
            move |Json(b): Json<Value>| {
                let s3 = s3.clone();
                async move {
                    s3.lock().unwrap().push(json!({"aurix_ban": b}));
                    Json(json!({"ban_id": "b1"}))
                }
            }
        }))
        .route("/v1/users/{id}/unban", post(|| async { Json(json!({"revoked": 1, "user_id": "u1"})) }))
        .route("/v1/users/{id}/blocks", post(|| async { Json(json!({"blocked": true})) }))
        .route("/v1/moderation/report", post({
            let s4 = seen.clone();
            move |Json(b): Json<Value>| {
                let s4 = s4.clone();
                async move {
                    s4.lock().unwrap().push(json!({"aurix_report": b}));
                    Json(json!({"id": "rep-1"}))
                }
            }
        }))
        .route("/v1/tokens", post(|Json(b): Json<Value>| async move {
            Json(json!({"token": format!("aurix-{}", b["external_id"].as_str().unwrap_or("")), "user_id": "u1", "expires_at": "2030-01-01T00:00:00Z",
                "channels": [{"channel_id": "chan-1", "name": b["channels"][0]["ad_hoc"]["name"]}], "endpoint": {"ws": "ws://voice/ws"}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test]
async fn zanshin_and_aurix_integrations() {
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    let (upstream, seen) = mock_upstreams().await;
    let liveops = musubi_integrations::ZanshinClient::new(&upstream, "zk_s_test");
    state.liveops = Some(liveops.clone());
    state.voice = Some(musubi_integrations::AurixClient::new(&upstream, "ak_test"));
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    // authentication is tracked as an analytics event
    let (ta, acc) = token(&router).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(liveops.flush().await, 1);
    let ev = seen.lock().unwrap().clone();
    assert_eq!(ev[0]["external_id"], acc.to_string());
    assert_eq!(ev[0]["events"][0]["name"], "musubi.authenticated");
    // flags are evaluated for the caller's identity
    let req = Request::get("/v1/liveops/flags?names=new_map")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["flags"][0]["key"], "new_map");
    assert_eq!(v["flags"][0]["external_id"], acc.to_string());
    // voice token only for members: create a party over the socket, then ask for its channel
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
        },
    )
    .await;
    let pid = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    let body =
        |pid: &str| Body::from(json!({"party_id": pid, "display_name": "Alice"}).to_string());
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body(&pid))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["token"], format!("aurix-{acc}"));
    assert_eq!(v["channel_id"], "chan-1");
    // a stranger is refused
    let (tb, _) = token(&router).await;
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body(&pid))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // a world's proximity channel: only a subscriber of the world (on this node) gets a token
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0}}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    send(
        &mut a,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let wbody = || Body::from(json!({"world": world, "display_name": "Alice"}).to_string());
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(wbody())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["channel_id"], "chan-1");
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(wbody())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // a voice report is filed in Aurix under the caller's identity
    let (_, acc_b) = token(&router).await;
    let req = Request::post("/v1/voice/report")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"target_account_id": acc_b, "reason": "slurs"}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);

    // an account ban is mirrored into Aurix
    let req = Request::post(format!("/v1/admin/accounts/{acc}/ban"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let ev = seen.lock().unwrap().clone();
    assert!(
        ev.iter().any(|e| e["aurix_report"]["reason"] == "slurs"),
        "{ev:?}"
    );
    assert!(
        ev.iter()
            .any(|e| e["aurix_ban"]["scope"] == "account" && e["aurix_ban"]["user_id"] == "u1"),
        "{ev:?}"
    );
}

/// A test PKI shaped like Apple's: root (P-384) → intermediate (P-384) → leaf (P-256, receipt
/// signing OID), plus a rogue root/leaf for negative cases.
struct TestChain {
    root: Vec<u8>,
    inter: Vec<u8>,
    leaf: Vec<u8>,
    leaf_key: Vec<u8>,
    rogue_root: Vec<u8>,
    rogue_leaf: Vec<u8>,
    rogue_key: Vec<u8>,
}

impl TestChain {
    fn generate() -> Self {
        use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair};
        let ca = |cn: &str| {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            p.distinguished_name.push(DnType::CommonName, cn);
            p
        };
        let leaf_params = || {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.distinguished_name
                .push(DnType::CommonName, "Test App Store Signing");
            p.custom_extensions.push(CustomExtension::from_oid_content(
                &[1, 2, 840, 113635, 100, 6, 11, 1],
                vec![0x05, 0x00],
            ));
            p
        };
        let root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let root = ca("Test Apple Root CA - G3")
            .self_signed(&root_key)
            .unwrap();
        let inter_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let inter = ca("Test Apple WWDR CA")
            .signed_by(&inter_key, &root, &root_key)
            .unwrap();
        let leaf_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let leaf = leaf_params()
            .signed_by(&leaf_key, &inter, &inter_key)
            .unwrap();
        let rogue_root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let rogue_root = ca("Rogue Root").self_signed(&rogue_root_key).unwrap();
        let rogue_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let rogue_leaf = leaf_params()
            .signed_by(&rogue_key, &rogue_root, &rogue_root_key)
            .unwrap();
        Self {
            root: root.der().to_vec(),
            inter: inter.der().to_vec(),
            leaf: leaf.der().to_vec(),
            leaf_key: leaf_key.serialize_der(),
            rogue_root: rogue_root.der().to_vec(),
            rogue_leaf: rogue_leaf.der().to_vec(),
            rogue_key: rogue_key.serialize_der(),
        }
    }

    /// `badsig`: genuine chain, signature from another key. `badchain`: chain to a rogue root.
    fn sign(&self, payload: &Value, id: &str) -> String {
        use base64::Engine;
        let e = base64::engine::general_purpose::STANDARD;
        let (chain, key) = match id {
            "badsig" => (vec![&self.leaf, &self.inter, &self.root], &self.rogue_key),
            "badchain" => (vec![&self.rogue_leaf, &self.rogue_root], &self.rogue_key),
            _ => (vec![&self.leaf, &self.inter, &self.root], &self.leaf_key),
        };
        let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        h.x5c = Some(chain.iter().map(|c| e.encode(c)).collect());
        jsonwebtoken::encode(&h, payload, &jsonwebtoken::EncodingKey::from_ec_der(key)).unwrap()
    }
}

async fn mock_stores(bundle_id: &str, item_product: &str) -> (String, Arc<TestChain>) {
    use axum::extract::Path;
    use axum::routing::{get, post};
    use axum::Json;
    let ch = Arc::new(TestChain::generate());
    let chain = ch.clone();
    let refunded: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Default::default();
    let (rf1, rf2) = (refunded.clone(), refunded.clone());
    let b = bundle_id.to_string();
    let prod = item_product.to_string();
    let app = axum::Router::new()
        .route(
            "/ISteamMicroTxnSandbox/InitTxn/v3/",
            post(|axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
                if f.get("key").map(String::as_str) != Some("STEAMKEY") || f.get("appid").map(String::as_str) != Some("480") {
                    return Json(json!({"response": {"result": "Failure", "error": {"errorcode": 3, "errordesc": "bad key"}}}));
                }
                let oid = f.get("orderid").cloned().unwrap_or_default();
                if f.get("amount[0]").map(String::as_str) != Some("398") || f.get("qty[0]").map(String::as_str) != Some("2") {
                    return Json(json!({"response": {"result": "Failure", "error": {"errorcode": 7, "errordesc": "bad amount"}}}));
                }
                Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}"), "steamurl": ""}}}))
            }),
        )
        .route(
            "/ISteamMicroTxnSandbox/FinalizeTxn/v2/",
            post(|axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
                let oid = f.get("orderid").cloned().unwrap_or_default();
                Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}")}}}))
            }),
        )
        .route(
            "/ISteamMicroTxnSandbox/QueryTxn/v3/",
            get(move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| {
                let rf = rf1.clone();
                async move {
                    let oid = q.get("orderid").cloned().unwrap_or_default();
                    let status = if rf.lock().unwrap().contains(&oid) { "Refunded" } else { "Succeeded" };
                    Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}"), "status": status}}}))
                }
            }),
        )
        .route(
            "/mock/steam/refund/{oid}",
            post(move |Path(oid): Path<String>| {
                let rf = rf2.clone();
                async move {
                    rf.lock().unwrap().insert(oid);
                    Json(json!({}))
                }
            }),
        )
        .route(
            "/inApps/v1/transactions/{id}",
            get(move |Path(id): Path<String>, headers: axum::http::HeaderMap| {
                let b = b.clone();
                let prod = prod.clone();
                let ch = ch.clone();
                async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if !auth.starts_with("Bearer ey") {
                        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    if id == "missing" {
                        return (axum::http::StatusCode::NOT_FOUND, Json(json!({})));
                    }
                    let payload = json!({
                        "transactionId": id, "originalTransactionId": id, "bundleId": if id == "foreign" { "com.other.app" } else { b.as_str() },
                        "productId": prod, "purchaseDate": 1_700_000_000_000i64, "quantity": 1, "environment": "Sandbox", "type": "Consumable",
                        "revocationDate": if id == "refunded" { json!(1_700_000_100_000i64) } else { Value::Null }
                    });
                    let jws = ch.sign(&payload, &id);
                    (axum::http::StatusCode::OK, Json(json!({"signedTransactionInfo": jws})))
                }
            }),
        )
        .route("/token", post(|body: String| async move {
            assert!(body.starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=ey"), "{body}");
            Json(json!({"access_token": "gtok", "expires_in": 3600, "token_type": "Bearer"}))
        }))
        .route(
            "/androidpublisher/v3/applications/{pkg}/purchases/products/{product}/tokens/{token}",
            get(|Path((pkg, _product, token)): Path<(String, String, String)>, headers: axum::http::HeaderMap| async move {
                assert_eq!(pkg, "com.musubi.test");
                assert_eq!(headers.get("authorization").unwrap(), "Bearer gtok");
                Json(json!({
                    "purchaseState": if token == "pending" { 2 } else { 0 }, "consumptionState": 0, "orderId": format!("GPA.{token}"),
                    "purchaseTimeMillis": "1700000000000", "purchaseType": 0, "quantity": 2
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, chain)
}

async fn http_json(
    router: &axum::Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    server_key: bool,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if server_key {
        req = req.header("x-musubi-server-key", "defaultkey");
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.unwrap();
    let st = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn iap_apple_and_google_receipts_redeem_once() {
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (gem_pack, gems_item, product) = (
        format!("pack-{suffix}"),
        format!("gems-{suffix}"),
        format!("com.musubi.gems100.{suffix}"),
    );
    let (upstream, apple_chain) = mock_stores("com.musubi.test", &product).await;
    let apple_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/apple_test_ec.pem"
    ))
    .unwrap();
    let google_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_test_rsa.pem"
    ))
    .unwrap();
    state.apple_iap = Some(
        musubi_integrations::AppleStoreClient::new_with_root(
            &upstream,
            "ISSUER",
            "KEYID",
            "com.musubi.test",
            &apple_pem,
            &apple_chain.root,
        )
        .unwrap(),
    );
    state.steam_iap =
        Some(musubi_integrations::SteamClient::new(&upstream, "STEAMKEY", 480, true).unwrap());
    state.google_iap = Some(
        musubi_integrations::GooglePlayClient::new_with_notifications(
            &upstream,
            &format!("{upstream}/token"),
            "com.musubi.test",
            "sa@test.iam",
            &google_pem,
            "notify-secret",
        )
        .unwrap(),
    );
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    // catalog: item + store listing + provider product mapping
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": gems_item, "name": "Gems", "stackable": true, "max_stack": 100000})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, b) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": gem_pack, "item_id": gems_item, "quantity": 100, "prices": {}})),
    )
    .await;
    assert_eq!(st, 200, "{b}");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/iap/products",
        None,
        true,
        Some(json!({"provider": "apple", "product_id": product, "store_item_id": "nope"})),
    )
    .await;
    assert_eq!(st, 404, "unknown store item");
    for prov in ["apple", "google"] {
        let (st, b) = http_json(
            &router,
            "POST",
            "/v1/server/iap/products",
            None,
            true,
            Some(json!({"provider": prov, "product_id": product, "store_item_id": gem_pack})),
        )
        .await;
        assert_eq!(st, 200, "{b}");
    }
    // apple: valid transaction grants 100 gems
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 100);
    assert_eq!(r["replayed"], false);
    assert_eq!(r["environment"], "Sandbox");
    // replay by the same account is idempotent, by another account is a conflict
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(r["replayed"], true);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&tb),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 409);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == gems_item)
            .unwrap()["quantity"],
        100
    );
    // rejected transactions: unknown, refunded, another app's bundle
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "missing"})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "refunded"})),
    )
    .await;
    assert_eq!(st, 403);

    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "badsig"})),
    )
    .await;
    assert_eq!(st, 403);

    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "badchain"})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "foreign"})),
    )
    .await;
    assert_eq!(st, 403);
    // google: pending purchase is refused, completed one grants quantity × listing
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/google",
        Some(&tb),
        false,
        Some(json!({"product_id": product, "purchase_token": "pending"})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/google",
        Some(&tb),
        false,
        Some(json!({"product_id": product, "purchase_token": format!("ok-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 200);
    assert_eq!(r["transaction_id"], format!("GPA.ok-{suffix}"));
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&tb), false, None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == gems_item)
            .unwrap()["quantity"],
        200
    );
    // receipts are listed for the player and for the server
    let (_, rc) = http_json(&router, "GET", "/v1/iap/receipts", Some(&ta), false, None).await;
    assert_eq!(rc["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(rc["receipts"][0]["provider"], "apple");
    let (st, rc) = http_json(
        &router,
        "GET",
        &format!("/v1/server/iap/receipts?account_id={acc_b}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{rc}");
    assert_eq!(rc["receipts"][0]["provider"], "google");
    // without a configured provider the endpoint says so
    state.apple_iap = None;
    let router2 = musubi_server::http::router(state);
    let (st, _) = http_json(
        &router2,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "x"})),
    )
    .await;
    assert_eq!(st, 404);
    // store-side refund: Apple notification revokes the receipt and takes the item back
    let tx_payload =
        json!({"transactionId": format!("tx-{suffix}"), "bundleId": "com.musubi.test"});
    let note = json!({"notificationType": "REFUND", "data": {"signedTransactionInfo": apple_chain.sign(&tx_payload, "ok")}});
    let signed = apple_chain.sign(&note, "ok");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": signed})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"]["transaction_id"], format!("tx-{suffix}"));
    assert_eq!(r["revoked"]["revoked"], true);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": signed})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(r["revoked"]["revoked"], true, "idempotent");
    let forged = apple_chain.sign(&note, "badchain");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": forged})),
    )
    .await;
    assert_eq!(st, 403);
    let (_, list) = http_json(&router, "GET", "/v1/iap/receipts", Some(&ta), false, None).await;
    assert!(list["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["transaction_id"] == format!("tx-{suffix}") && x["revoked"] == true));
    // Google RTDN (Pub/Sub push) voided purchase
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(
        json!({"voidedPurchaseNotification": {"purchaseToken": format!("ok-{suffix}")}})
            .to_string(),
    );
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/google/notifications?token=wrong",
        None,
        false,
        Some(json!({"message": {"data": data}})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/google/notifications?token=notify-secret",
        None,
        false,
        Some(json!({"message": {"data": data}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"]["revoked"], true);
    assert_eq!(r["revoked"]["provider"], "google");
    // Steam: server-initiated order → overlay → finalize grants; refunds found by reconciliation
    let (st, _) = http_json(&router, "POST", "/v1/server/iap/products", None, true, Some(json!({"provider": "steam", "product_id": "7001", "store_item_id": gem_pack, "amount_cents": 199, "description": "Gem pack"}))).await;
    assert_eq!(st, 200);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/init",
        Some(&ta),
        false,
        Some(json!({"steam_id": "76561197960287930", "store_item_id": gem_pack, "quantity": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let order_id = r["order_id"].as_str().unwrap().to_string();
    assert_eq!(r["transaction_id"], format!("T{order_id}"));
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/init",
        Some(&ta),
        false,
        Some(json!({"steam_id": "1", "store_item_id": "nope", "quantity": 1})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/finalize",
        Some(&ta),
        false,
        Some(json!({"order_id": order_id})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["provider"], "steam");
    assert_eq!(r["transaction_id"], format!("T{order_id}"));
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/finalize",
        Some(&ta),
        false,
        Some(json!({"order_id": order_id})),
    )
    .await;
    assert_eq!(st, 409);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"], json!([]));
    reqwest::Client::new()
        .post(format!("{upstream}/mock/steam/refund/{order_id}"))
        .send()
        .await
        .unwrap();
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(r["revoked"], json!([format!("T{order_id}")]));
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(
        r["revoked"],
        json!([]),
        "already revoked receipts are skipped"
    );
}

/// Persistent entities survive the home node: registry 2 adopts the world from the snapshot,
/// becomes its home and keeps only what was marked persistent.
#[tokio::test]
async fn world_snapshots_persist_and_recover_on_another_node() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let (n1, n2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("w-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    let keep = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos { x: 1.0, y: 1.0 },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.spawn(
        &name,
        sim,
        musubi_mesh::Pos { x: 2.0, y: 2.0 },
        json!({}),
        false,
    )
    .unwrap();
    assert_eq!(r1.info(&name).unwrap().entities, 2);
    r1.snapshot_now(&name).await.unwrap();

    let r2 = musubi_server::realtime::WorldRegistry::new(n2, Some(db.clone())).with_group(&group);
    // n1 never heartbeated into musubi.nodes, so the world counts as orphaned even without an
    // explicit dead list (full-cluster restart case)
    let adopted = r2.recover(&[]).await.unwrap();
    assert_eq!(adopted, vec![name.clone()]);
    assert!(r2.recover(&[n1]).await.unwrap().is_empty(), "already ours");
    assert_eq!(r2.home_of(&name).await.unwrap(), Some(n2));
    let info = r2.info(&name).unwrap();
    assert_eq!(info.entities, 1, "only the persistent entity is restored");
    let cell = musubi_mesh::CellId { x: 0, y: 0 };
    let view = r2.view(&name, cell).unwrap();
    let s = view.to_string();
    assert!(s.contains(&keep.id.to_string()), "{s}");
    assert!(s.contains("\"hp\""), "{s}");
    // n2 is a live node now, so a third registry has nothing left to claim
    sqlx::query("insert into musubi.nodes (id, name, cluster_group) values ($1, 'n2', $2)")
        .bind(n2)
        .bind(name.clone())
        .execute(&db)
        .await
        .unwrap();
    let r3 = musubi_server::realtime::WorldRegistry::new(uuid::Uuid::now_v7(), Some(db.clone()))
        .with_group(&group);
    assert!(r3.recover(&[n1]).await.unwrap().is_empty());
    r2.delete(&name).await.unwrap();
    sqlx::query("delete from musubi.nodes where id = $1")
        .bind(n2)
        .execute(&db)
        .await
        .unwrap();
}

/// Admin kick closes the player's live sockets with a `kicked` error.
#[tokio::test]
async fn admin_kick_closes_realtime_sessions() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(&mut a, "json", None, ClientMessage::Ping).await;
    recv(&mut a).await;
    let req = Request::post(format!("/v1/admin/accounts/{acc}/kick"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["kicked_local"], 1);
    let env = recv(&mut a).await;
    assert!(
        matches!(&env.msg, ServerMessage::Error { message, .. } if message.contains("kicked")),
        "{:?}",
        env.msg
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let req = Request::get("/v1/node").body(Body::empty()).unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["realtime_sessions"], 0, "socket closed and left the hub");
}

/// Consumption gossip makes a rate-limit budget cluster-wide: an account that spends its burst on
/// node 1 is refused on node 2 once the usage has travelled over the bus.
#[tokio::test]
async fn rate_limits_are_cluster_wide_over_the_bus() {
    let group = test_group();
    let Some(mut n1) = build_state(false, &group).await else {
        return;
    };
    let Some(mut n2) = build_state(false, &group).await else {
        return;
    };
    let cfg = musubi_core::config::LimitsConfig {
        enabled: true,
        http_per_second_per_account: 1,
        http_burst: 10,
        cluster_wide: true,
        ..Default::default()
    };
    for n in [&mut n1, &mut n2] {
        n.limits = Arc::new(musubi_server::limits::RateLimiter::new(&cfg));
        n.limits.start_gossip(n.cluster.clone().unwrap());
    }
    // let the two buses discover each other
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let alive = n1.cluster.as_ref().unwrap().nodes().await.unwrap().len();
        if alive >= 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "peers never met");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let id = musubi_core::AccountId(uuid::Uuid::now_v7());
    for _ in 0..10 {
        n1.limits.account(id).unwrap();
    }
    assert!(n1.limits.account(id).is_err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if n2.limits.account(id).is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "node 2 never learned node 1's consumption"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    // an unrelated account on node 2 is unaffected
    assert!(n2
        .limits
        .account(musubi_core::AccountId(uuid::Uuid::now_v7()))
        .is_ok());
}

/// `/readyz` reflects the real state: ready once the database, migrations and Raft leader are in
/// place; 503 with `draining=true` once the node starts draining.
#[tokio::test]
async fn readyz_reports_db_migrations_raft_and_draining() {
    let group = test_group();
    let Some(state) = build_state(false, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let probe = |router: axum::Router| async move {
        let res = router
            .oneshot(
                axum::http::Request::get("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status().as_u16();
        let body: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        (status, body)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let body = loop {
        let (status, body) = probe(router.clone()).await;
        assert_eq!(body["checks"]["db"], true, "{body}");
        assert_eq!(body["checks"]["migrations"], true, "{body}");
        assert_eq!(body["checks"]["draining"], false, "{body}");
        if status == 200 {
            break body;
        }
        assert_eq!(body["checks"]["raft"], false, "only raft may lag: {body}");
        assert!(std::time::Instant::now() < deadline, "never ready: {body}");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    assert_eq!(body["status"], "ok");
    // null = Raft not enabled on this node; true = leader known
    let raft = &body["checks"]["raft"];
    assert!(raft.is_null() || *raft == true, "{body}");
    state
        .draining
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (status, body) = probe(router).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["checks"]["draining"], true);
    assert_eq!(body["status"], "unavailable");
}

/// Admin drain: the local node's shutdown fires directly; another node in the group is reached
/// over the bus. Cluster-wide match/world views come from the database.
#[tokio::test]
async fn admin_drain_reaches_local_and_remote_nodes() {
    let group = test_group();
    let Some(n1) = build_state(false, &group).await else {
        return;
    };
    let Some(n2) = build_state(false, &group).await else {
        return;
    };
    musubi_server::http::admin::start_drain_listener(n2.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if n1
            .cluster
            .as_ref()
            .unwrap()
            .nodes()
            .await
            .unwrap()
            .iter()
            .filter(|n| n.alive)
            .count()
            >= 2
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "peers never met");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let router = musubi_server::http::router(n1.clone());
    let post = |router: axum::Router, id: uuid::Uuid| async move {
        let res = router
            .oneshot(
                axum::http::Request::post(format!("/v1/admin/cluster/nodes/{id}/drain"))
                    .header("x-musubi-server-key", "defaultkey")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let st = res.status().as_u16();
        let v: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap_or_default();
        (st, v)
    };
    let (st, v) = post(router.clone(), uuid::Uuid::now_v7()).await;
    assert_eq!(st, 404, "{v}");

    let n2_id = n2.cluster.as_ref().unwrap().node_id().0;
    let notified = n2.shutdown.clone();
    let waiter = tokio::spawn(async move { notified.notified().await });
    let (st, v) = post(router.clone(), n2_id).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["local"], false);
    tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
        .await
        .expect("remote node receives the drain")
        .unwrap();

    let n1_id = n1.cluster.as_ref().unwrap().node_id().0;
    let notified = n1.shutdown.clone();
    let waiter = tokio::spawn(async move { notified.notified().await });
    let (st, v) = post(router.clone(), n1_id).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["local"], true);
    tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();

    let res = router
        .oneshot(
            axum::http::Request::get("/v1/admin/cluster/worlds")
                .header("x-musubi-server-key", "defaultkey")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

/// SWIM membership changes become outbox events: a second node joining the group shows up as
/// `node.joined` for the first one.
#[tokio::test]
async fn cluster_membership_changes_reach_the_outbox() {
    let group = test_group();
    let Some(n1) = build_state(false, &group).await else {
        return;
    };
    musubi_server::events::start_cluster_events(n1.clone());
    let Some(n2) = build_state(false, &group).await else {
        return;
    };
    let n2_id = n2.cluster.as_ref().unwrap().node_id().0.to_string();
    let router = musubi_server::http::router(n1.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    loop {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::get("/v1/server/events?kinds=node.joined&limit=200")
                    .header("x-musubi-server-key", "defaultkey")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        let seen = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["payload"]["node_id"] == n2_id);
        if seen {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no node.joined for n2: {v}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// Ops after the last snapshot are journaled and replayed when another node adopts the world.
#[tokio::test]
async fn world_journal_is_replayed_on_rehome() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let n1 = uuid::Uuid::now_v7();
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("wj-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    let keep = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos { x: 1.0, y: 1.0 },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.snapshot_now(&name).await.unwrap();
    // after the snapshot: damage the first entity, spawn a second persistent one, despawn a third
    r1.apply(&name, sim, &keep.id.to_string(), None, json!({"hp": 5}))
        .unwrap();
    let late = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos { x: 1.5, y: 1.5 },
            json!({"kind": "chest"}),
            true,
        )
        .unwrap();
    let gone = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos { x: 1.2, y: 1.2 },
            json!({}),
            true,
        )
        .unwrap();
    r1.despawn(&name, sim, &gone.id.to_string()).unwrap();
    r1.flush_journal(&name).await.unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from musubi.world_journal where name = $1")
        .bind(&name)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 1);

    let r2 = musubi_server::realtime::WorldRegistry::new(uuid::Uuid::now_v7(), Some(db.clone()))
        .with_group(&group);
    assert_eq!(r2.recover(&[]).await.unwrap(), vec![name.clone()]);
    assert_eq!(r2.info(&name).unwrap().entities, 2, "keep + late, not gone");
    let view = r2
        .view(&name, musubi_mesh::CellId { x: 0, y: 0 })
        .unwrap()
        .to_string();
    assert!(
        view.contains(&keep.id.to_string())
            && view.contains(&late.id.to_string())
            && !view.contains(&gone.id.to_string()),
        "{view}"
    );
    assert!(view.contains("\"name\":\"hp\",\"value\":5"), "{view}");
    assert!(view.contains("chest"), "{view}");
    r2.delete(&name).await.unwrap();
}

/// A session opened with `resumable=true` survives losing its socket: subscriptions and the
/// outbound buffer are kept for `realtime.resume_ttl_secs`, and `?resume=<token>` re-attaches.
#[tokio::test]
async fn lost_socket_is_resumed_with_its_token() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let (mut a, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?token={ta}&format=json&resumable=true"
    ))
    .await
    .unwrap();
    let hello = recv(&mut a).await.msg;
    let ServerMessage::Session {
        session_id,
        resume_token,
        resumed: false,
    } = hello
    else {
        panic!("expected session hello, got {hello:?}");
    };
    send(
        &mut a,
        "json",
        Some("f"),
        ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Status { .. }
    ));
    let mut b = connect(&addr, &tb, "json").await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));

    // lose the socket without a close frame
    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("back".into()),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // wrong token: rejected and closed
    let (mut bad, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?resume=nope&format=json"))
            .await
            .unwrap();
    assert!(matches!(
        recv(&mut bad).await.msg,
        ServerMessage::Error { .. }
    ));

    let (mut a2, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    let m = recv(&mut a2).await.msg;
    assert_eq!(
        m,
        ServerMessage::Session {
            session_id: session_id.clone(),
            resume_token: resume_token.clone(),
            resumed: true
        }
    );
    // the presence event that happened while we were gone was buffered
    match recv(&mut a2).await.msg {
        ServerMessage::StatusPresenceEvent { joins, .. } => {
            assert!(
                joins.iter().any(|p| p.status.as_deref() == Some("back")),
                "{joins:?}"
            );
        }
        other => panic!("expected buffered presence event, got {other:?}"),
    }
    send(&mut a2, "json", Some("p"), ClientMessage::Ping).await;
    assert_eq!(recv(&mut a2).await.msg, ServerMessage::Pong);
    // the old token is single-use: a second resume is rejected
    let (mut again, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    assert!(matches!(
        recv(&mut again).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// QUIC sessions resume the same way as WebSocket ones: hello with `resumable`, lose the
/// connection, hello again with `resume`.
#[tokio::test]
async fn quic_session_resumes_with_its_token() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr = musubi_server::realtime::quic::start_with(
        state.clone(),
        "127.0.0.1:0".parse().unwrap(),
        cfg,
    )
    .await
    .unwrap();
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json", "resumable": true}).to_string()),
    )
    .await
    .unwrap();
    let ServerMessage::Session {
        resume_token,
        resumed: false,
        ..
    } = quic_read(&mut rx).await
    else {
        panic!("expected session hello")
    };
    let env = |msg: ClientMessage| {
        Frame::Text(serde_json::to_string(&Envelope { cid: None, msg }).unwrap())
    };
    Q::write_frame(
        &mut tx,
        env(ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        quic_read(&mut rx).await,
        ServerMessage::Status { .. }
    ));
    let mut b = connect(&router_addr(&state).await, &tb, "json").await;
    let _ = &mut b;
    assert!(matches!(
        quic_read(&mut rx).await,
        ServerMessage::StatusPresenceEvent { .. }
    ));

    // lose the QUIC connection without a close frame
    conn.close(0u32.into(), b"gone");
    drop(conn);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("back".into()),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let conn2 = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap();
    Q::write_frame(
        &mut tx2,
        Frame::Text(json!({"resume": resume_token, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    assert!(matches!(
        quic_read(&mut rx2).await,
        ServerMessage::Session { resumed: true, .. }
    ));
    match quic_read(&mut rx2).await {
        ServerMessage::StatusPresenceEvent { joins, .. } => {
            assert!(
                joins.iter().any(|p| p.status.as_deref() == Some("back")),
                "{joins:?}"
            );
        }
        other => panic!("expected buffered presence event, got {other:?}"),
    }
    Q::write_frame(&mut tx2, env(ClientMessage::Ping))
        .await
        .unwrap();
    assert_eq!(quic_read(&mut rx2).await, ServerMessage::Pong);
}

/// Serve the HTTP router of an in-process state on a free port (for WS peers in QUIC tests).
async fn router_addr(state: &musubi_server::app::AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = musubi_server::http::router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr.to_string()
}

async fn quic_read(rx: &mut quinn::RecvStream) -> ServerMessage {
    let musubi_transport::Frame::Text(t) =
        musubi_transport::quic::QuicConnection::read_frame(rx, 1 << 20)
            .await
            .unwrap()
    else {
        panic!("expected text frame")
    };
    serde_json::from_str::<Envelope<ServerMessage>>(&t)
        .unwrap()
        .msg
}

/// A ticket past the TTL is dropped and its owner told, so clients can re-queue or widen criteria.
#[tokio::test]
async fn matchmaker_ticket_expires_with_notice() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        Some("t"),
        ClientMessage::MatchmakerAdd {
            min_count: 2,
            max_count: 2,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    let ServerMessage::MatchmakerTicket { ticket } = recv(&mut a).await.msg else {
        panic!("expected ticket")
    };
    let (hub, parties, mm) = (
        state.hub.clone(),
        state.parties.clone(),
        state.matchmaker.clone(),
    );
    let expired = mm.expire_older_than(std::time::Duration::ZERO);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, ticket);
    musubi_server::realtime::matchmaking::deliver_expired(&hub, &parties, expired);
    assert_eq!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerExpired { ticket }
    );
    assert_eq!(mm.ticket_count(), 0);
}

/// A session on node 2 makes the account "online anywhere" on node 1 (so pushes are not sent to
/// players who are connected elsewhere); disconnecting clears it.
#[tokio::test]
async fn presence_is_known_cluster_wide() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr2 = router_addr(&state2).await;
    let (tb, acc_b) = token(&router1).await;
    let acc: musubi_core::AccountId = musubi_core::AccountId(acc_b.parse().unwrap());
    assert!(!state1.fanout.is_online_anywhere(acc));
    let mut b = connect(&addr2, &tb, "json").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 never learned about the session on node 2"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(state2.fanout.is_online_anywhere(acc));
    let _ = b.close(None).await;
    drop(b);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 still thinks the account is online"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Send a room message; true if accepted (ack + broadcast arrive), false if rejected with an error.
async fn say(a: &mut Ws, f: &str, room_id: &str, text: &str) -> bool {
    send(
        a,
        f,
        Some("m"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.to_string(),
            content: serde_json::json!({"text": text}),
        },
    )
    .await;
    if matches!(recv(a).await.msg, ServerMessage::Error { .. }) {
        return false;
    }
    assert!(!matches!(recv(a).await.msg, ServerMessage::Error { .. }));
    true
}

/// Muted accounts cannot send (but stay in the channel), banned accounts cannot join; lifting
/// the restriction restores both.
#[tokio::test]
async fn chat_moderation_mutes_and_bans() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (tok, acc) = token(&router).await;
    let acc = musubi_core::AccountId(acc.parse().unwrap());
    let f = "json";
    let mut a = connect(&addr, &tok, f).await;
    let room = format!("mod-{}", uuid::Uuid::now_v7().simple());
    send(
        &mut a,
        f,
        Some("j"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let room_id = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    let channel = musubi_chat::Channel::parse(&room_id).unwrap();
    let mut a2 = a;
    assert!(say(&mut a2, f, &room_id, "hello").await);
    state
        .chat
        .moderate(&channel, acc, "mute", Some("spam"), Some("test"), None)
        .await
        .unwrap();
    assert!(!say(&mut a2, f, &room_id, "muted?").await);
    let mods = state.chat.moderations(&channel).await.unwrap();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].kind, "mute");
    assert!(state.chat.unmoderate(&channel, acc).await.unwrap());
    assert!(say(&mut a2, f, &room_id, "free again").await);
    // An expired timed mute does not restrict.
    state
        .chat
        .moderate(&channel, acc, "mute", None, None, Some(0))
        .await
        .unwrap();
    assert!(state
        .chat
        .restriction(&channel, acc)
        .await
        .unwrap()
        .is_none());
    // Ban: kicked out of the channel, cannot rejoin.
    state
        .chat
        .moderate(&channel, acc, "ban", None, None, None)
        .await
        .unwrap();
    state.fanout.leave_channel_everywhere(&room_id, acc);
    assert!(!say(&mut a2, f, &room_id, "banned?").await);
    send(
        &mut a2,
        f,
        Some("j2"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// A node that leaves the cluster takes its sessions out of the cluster-wide online set.
#[tokio::test]
async fn presence_purged_when_node_leaves() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr2 = router_addr(&state2).await;
    let (tb, acc_b) = token(&router1).await;
    let acc = musubi_core::AccountId(acc_b.parse().unwrap());
    let _b = connect(&addr2, &tb, "json").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 never saw the session"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    state2.cluster.as_ref().unwrap().leave().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 kept the left node's sessions"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A resumable session whose node is gone (or just not the one the client reached) is rebuilt on
/// the node that receives `?resume=`: same session id, same token, follows replayed, and the old
/// parked copy is stolen from the node that held it.
#[tokio::test]
async fn resume_rebuilds_session_on_another_node() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr1 = router_addr(&state1).await;
    let addr2 = router_addr(&state2).await;
    let (ta, _) = token(&router1).await;
    let (tb, acc_b) = token(&router1).await;
    let (mut a, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr1}/ws?token={ta}&format=json&resumable=true"
    ))
    .await
    .unwrap();
    let ServerMessage::Session {
        session_id,
        resume_token,
        resumed: false,
    } = recv(&mut a).await.msg
    else {
        panic!("expected session hello");
    };
    send(
        &mut a,
        "json",
        Some("f"),
        ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Status { .. }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let rows: i64 =
        sqlx::query_scalar("select count(*) from musubi.resumable_sessions where session_id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&state1.db)
            .await
            .unwrap();
    assert_eq!(rows, 1, "record written");

    // transport lost on node 1: the session parks there and its record points at node 1
    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(state1.hub.parked_count(), 1);

    // the client comes back through node 2
    let (mut a2, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr2}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    let hello = recv(&mut a2).await.msg;
    let ServerMessage::Session {
        session_id: sid2,
        resume_token: tok2,
        resumed: true,
    } = hello
    else {
        panic!("expected resumed hello, got {hello:?}");
    };
    assert_eq!(sid2, session_id);
    assert_eq!(tok2, resume_token);
    // the follow was replayed...
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::Status { .. }
    ));
    // ...so b's status change reaches the rebuilt session
    let mut b = connect(&addr1, &tb, "json").await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("moved".into()),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));
    // node 1 dropped its parked copy
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.hub.parked_count() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "parked copy not stolen"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Reactions: HTTP add/remove on a message, live `ChannelMessageReaction` to channel members,
/// aggregated `reactions` (with `me`) in history.
#[tokio::test]
async fn chat_message_reactions() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    // reactions live on persisted messages, so use a direct channel (room messages are not stored)
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    let room = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_a.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Channel { .. }
    ));
    // a may or may not get a presence event for b: drain it if it comes
    let _ = tokio::time::timeout(std::time::Duration::from_millis(400), a.next()).await;
    send(
        &mut a,
        f,
        Some("m1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room.clone(),
            content: serde_json::json!({"text": "react to me"}),
        },
    )
    .await;
    let mut mid = None;
    for _ in 0..2 {
        if let ServerMessage::ChannelMessageAck { message_id, .. } = recv(&mut a).await.msg {
            mid = Some(message_id);
        }
    }
    let mid = mid.unwrap();
    recv(&mut b).await; // the message itself

    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&tb),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["count"], 1);
    // both members see the live event
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::ChannelMessageReaction {
                message_id,
                emoji,
                account_id,
                added,
                count,
                ..
            } => {
                assert_eq!(message_id, mid);
                assert_eq!(emoji, "🔥");
                assert_eq!(account_id.to_string(), acc_b);
                assert!(added);
                assert_eq!(count, 1);
            }
            other => panic!("expected reaction event, got {other:?}"),
        }
    }
    // idempotent re-add, then a's own reaction makes two
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&tb),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(r["count"], 1, "{r}");
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(r["count"], 2, "{r}");
    // history aggregates per emoji and marks the viewer's own
    let (st, h) = http_json(
        &router,
        "GET",
        &format!("/v1/channel/{room}/messages"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{h}");
    let m = h["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["message_id"] == mid.as_str())
        .unwrap();
    assert_eq!(
        m["reactions"],
        serde_json::json!([{"emoji": "🔥", "count": 2, "me": true}]),
        "{h}"
    );
    // remove
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/channel/{room}/messages/{mid}/reactions/%F0%9F%94%A5"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["count"], 1);
    // bad inputs
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": ""})),
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = http_json(
        &router,
        "POST",
        &format!(
            "/v1/channel/{room}/messages/{}/reactions",
            uuid::Uuid::nil()
        ),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": "x"})),
    )
    .await;
    assert_eq!(st, 404);
}

/// A member invites an account; the invitee's sessions get `PartyInvite` and can join a closed
/// party without the leader's approval. Outsiders cannot invite.
#[tokio::test]
async fn party_invite_lets_invitee_join_closed_party() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: false,
            max_size: 4,
        },
    )
    .await;
    let party_id = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    let acc_b_id: musubi_core::AccountId = acc_b.parse().unwrap();
    // an outsider cannot invite
    send(
        &mut c,
        f,
        Some("x"),
        ClientMessage::PartyInvite {
            party_id: party_id.clone(),
            account_id: acc_b_id,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
    // a member can
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyInvite {
            party_id: party_id.clone(),
            account_id: acc_b_id,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::PartyInvite { party_id: p, from } => {
            assert_eq!(p, party_id);
            assert_ne!(from.account_id, acc_b_id);
        }
        m => panic!("{m:?}"),
    }
    // the invitee joins the closed party directly
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: party_id.clone(),
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::Party { presences, .. } => assert_eq!(presences.len(), 2),
        m => panic!("{m:?}"),
    }
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::PartyPresenceEvent { joins, .. } if joins.len() == 1
    ));
    // c, never invited, only gets queued as a request (no snapshot) — the leader sees the request
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: party_id.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::PartyJoinRequest { .. }
    ));
}

/// One cell of a world is assigned to another node: spawns into that cell are redirected to and
/// served by the cell host, the rest of the world stays on the home node.
#[tokio::test]
async fn world_cell_hosted_on_another_node() {
    let group = format!("xcell-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim = uuid::Uuid::now_v7();
    let spawn = serde_json::json!({"sim": sim, "x": 150.0, "y": 10.0, "components": {}, "persistent": false});
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(spawn.clone()),
    )
    .await;
    assert!(st == 307 || st == 409, "home node redirects spawns into a cell hosted elsewhere (307, or 409 without an advertised address): {st}");
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert!(st == 200 || st == 409, "{l}");
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(spawn),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let (st, view) = http_json(
        &router2,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    assert_eq!(view["owned"].as_array().map(|a| a.len()), Some(1), "{view}");
    let (st, _) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert!(
        st == 307 || st == 409,
        "home node redirects views of the hosted cell: {st}"
    );
    let (st, _) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/0/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "other cells stay on the home node");
}

/// Cells of one world on two nodes: entities near the shared border show up as ghosts in the view
/// of the neighbouring cell on the other node (published over the bus every tick).
#[tokio::test]
async fn world_ghosts_cross_nodes() {
    let group = format!("xghost-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    // A hosts (0,0): one entity at the border (x=95), one deep inside (x=10).
    // B hosts (1,0): one entity at the border (x=110).
    let mut ids = Vec::new();
    for (router, sim, x) in [
        (&router1, sim_a, 95.0),
        (&router1, sim_a, 10.0),
        (&router2, sim_b, 110.0),
    ] {
        let (st, e) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/spawn"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "x": x, "y": 50.0, "components": {}, "persistent": false})),
        )
        .await;
        assert_eq!(st, 200, "{e}");
        ids.push(
            e["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no id: {e}"))
                .to_string(),
        );
    }
    let ghost_ids = |v: &Value| -> Vec<String> {
        v["ghosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect()
    };
    let mut ok = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if ghost_ids(&va) == vec![ids[2].clone()] && ghost_ids(&vb) == vec![ids[0].clone()] {
            ok = true;
            break;
        }
    }
    assert!(ok, "ghosts did not cross the node border");
}

/// An entity that crosses into a cell hosted on another node is handed over there: it leaves the
/// first node's shard and shows up as owned on the second.
#[tokio::test]
async fn world_handover_cross_nodes() {
    let group = format!("xhand-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "x": 95.0, "y": 50.0, "components": {"hp": 7}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id, "x": 130.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let owned_ids = |v: &Value| -> Vec<String> {
        v["owned"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect()
    };
    let mut ok = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if owned_ids(&va).is_empty() && owned_ids(&vb) == vec![id.clone()] {
            let e = &vb["owned"][0];
            let hp = e["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == "hp")
                .map(|c| c["value"].clone());
            assert_eq!(
                hp,
                Some(serde_json::json!(7)),
                "state travels with the entity: {e}"
            );
            ok = true;
            break;
        }
    }
    assert!(ok, "entity was not handed over to the other node");
    // and back again: the home's cells have no explicit host entry, the host must still hand over
    let (st, a) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "entity": id, "x": 70.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let mut back = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if owned_ids(&va) == vec![id.clone()] && owned_ids(&vb).is_empty() {
            back = true;
            break;
        }
    }
    assert!(back, "entity did not come back to the home node");
}

/// The node hosting a cell dies: the home node takes the cell back and restores the dead host's
/// persistent entities from that host's own snapshot.
#[tokio::test]
async fn world_cell_host_failover() {
    let group = format!("xfail-{}", uuid::Uuid::now_v7().simple());
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let router2 = musubi_server::http::router(state2.clone());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let node2 = state2.worlds.node_id();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim_b = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    // leasing B's cell through A is routed to B, not served by the home
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert!(
        st == 307 || st == 409,
        "lease of a cell hosted elsewhere is redirected: {st}"
    );
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "x": 150.0, "y": 50.0, "components": {"kind": "chest"}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    state2.worlds.snapshot_now(&name).await.unwrap();
    let rows: Vec<(uuid::Uuid,)> =
        sqlx::query_as("select node_id from musubi.world_snapshots where name = $1")
            .bind(&name)
            .fetch_all(&state2.db)
            .await
            .unwrap();
    assert!(
        rows.iter().any(|r| r.0 == node2),
        "B wrote its own shard snapshot: {rows:?}"
    );
    // B is gone: the home re-homes the cell and restores B's shard
    let cells = state1
        .worlds
        .recover_cells(&[node2], &state1.fanout)
        .await
        .unwrap();
    assert_eq!(
        cells,
        vec![(name.clone(), musubi_mesh::CellId { x: 1, y: 0 })]
    );
    let (st, va) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "home serves the cell again: {va}");
    let owned: Vec<&str> = va["owned"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["id"].as_str())
        .collect();
    assert_eq!(owned, vec![id.as_str()], "{va}");
    let sim_c = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_c, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "the cell is leasable on the home again");
    let sim_a = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    // both cells now live on the home: a border crossing is a local handover that must reach
    // the target simulator as handover_request and complete on its ack
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_c, "entity": id, "x": 50.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let mut requested = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (st, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim_a}"),
            None,
            true,
            None,
        )
        .await;
        assert_eq!(st, 200, "{ev}");
        if ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == id.as_str())
        {
            requested = true;
            break;
        }
    }
    assert!(requested, "sim_a was asked to take the entity");
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/handover/ack"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let mut owned_home = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (_, v0) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        if v0["owned"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == id.as_str())
        {
            owned_home = true;
            break;
        }
    }
    assert!(owned_home, "entity completed the local handover into (0,0)");
}

/// Three nodes: A is the world's home with cell (0,0), B hosts cell (1,0), a client connected to C
/// subscribes across both cells. It receives the entity from A, and after the entity crosses the
/// border it ends up knowing it exactly once, at its new position, streamed by B.
#[tokio::test]
async fn world_interest_across_nodes() {
    let group = format!("xint-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    let Some((addr3, _router3)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let (tc, _) = token(&router1).await;
    let mut c = connect(&addr3, &tc, "json").await;
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: name.clone(),
            x: 100.0,
            y: 50.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "x": 95.0, "y": 50.0, "components": {"hp": 7}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    let mut known: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let apply_delta = |known: &mut std::collections::HashMap<String, f64>, m: ServerMessage| {
        if let ServerMessage::WorldDelta {
            spawned,
            updated,
            despawned,
            ..
        } = m
        {
            for d in despawned {
                known.remove(&d.to_string());
            }
            for e in spawned.into_iter().chain(updated) {
                known.insert(e.id.to_string(), e.x);
            }
        }
    };
    // the client on C learns about the entity from A
    for _ in 0..10 {
        let m = recv(&mut c).await.msg;
        apply_delta(&mut known, m);
        if known.get(&id) == Some(&95.0) {
            break;
        }
    }
    assert_eq!(known.get(&id), Some(&95.0), "{known:?}");
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id, "x": 130.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    // after the handover B streams it; A's shard reports it gone. Whatever the interleaving, the
    // client converges on one copy at the new position.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut converged = false;
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(std::time::Duration::from_millis(300), c.next()).await;
        match next {
            Ok(Some(Ok(frame))) => {
                if let Ok(env) =
                    serde_json::from_slice::<Envelope<ServerMessage>>(&frame.into_data())
                {
                    apply_delta(&mut known, env.msg);
                }
            }
            Ok(_) => break,
            Err(_) => {
                if known.len() == 1 && known.get(&id) == Some(&130.0) {
                    converged = true;
                    break;
                }
            }
        }
    }
    assert!(
        converged,
        "client did not converge on the moved entity: {known:?}"
    );
    let (_, vb) = http_json(
        &router2,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(vb["owned"].as_array().map(|a| a.len()), Some(1), "{vb}");
}

/// Streaming simulator protocol: one WebSocket per node replaces per-tick `/view` + `/events`
/// polling — the node pushes a `tick` frame after every registry tick and renews the lease while
/// the socket lives; commands reuse the HTTP bodies.
#[tokio::test]
async fn simulator_stream_pushes_views_every_tick() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let Some((addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 2.0}})
                .to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let sim = uuid::Uuid::now_v7();
    let mut creq = format!("ws://{addr}/v1/server/worlds/{world}/stream?sim={sim}")
        .into_client_request()
        .unwrap();
    creq.headers_mut()
        .insert("x-musubi-server-key", "defaultkey".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(creq).await.unwrap();
    async fn next(ws: &mut Ws) -> Value {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    async fn reply(ws: &mut Ws, op: &str) -> Value {
        let t0 = std::time::Instant::now();
        loop {
            let v = next(ws).await;
            if v["op"] == op {
                return v;
            }
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(5),
                "no `{op}` reply: {v}"
            );
        }
    }
    ws.send(Message::Text(
        json!({"op": "lease", "id": 1, "cells": [{"x": 0, "y": 0}]}).to_string(),
    ))
    .await
    .unwrap();
    let r = reply(&mut ws, "lease").await;
    assert_eq!(r["id"], 1, "{r}");
    assert_eq!(r["cells"].as_array().unwrap().len(), 1, "{r}");
    ws.send(Message::Text(
        json!({"op": "spawn", "id": 2, "x": 50.0, "y": 50.0, "components": {"hp": 7}}).to_string(),
    ))
    .await
    .unwrap();
    let r = reply(&mut ws, "spawn").await;
    let id = r["entity"]["id"].clone();
    assert!(!id.is_null(), "{r}");
    // the next tick frames carry the entity in the leased cell's view, without any request
    let t0 = std::time::Instant::now();
    let first = loop {
        let v = next(&mut ws).await;
        if v["type"] == "tick" {
            let cells = v["cells"].as_array().cloned().unwrap_or_default();
            let seen = cells.iter().any(|c| {
                c["cell"] == json!({"x": 0, "y": 0})
                    && c["owned"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|e| e["id"] == id))
            });
            if seen {
                break v["tick"].as_u64().unwrap();
            }
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "entity never streamed: {v}"
        );
    };
    let later = loop {
        let v = next(&mut ws).await;
        if v["type"] == "tick" {
            break v["tick"].as_u64().unwrap();
        }
    };
    assert!(later > first, "{later} > {first}");
    ws.send(Message::Text(json!({"op": "nope", "id": 3}).to_string()))
        .await
        .unwrap();
    let r = reply(&mut ws, "nope").await;
    assert_eq!(r["id"], 3, "{r}");
    assert!(r["error"].as_str().unwrap().contains("unknown op"), "{r}");
}

/// A simulator hits an entity it does not own: locally (another cell on the same node) and from
/// the node hosting a neighbouring cell. The owner's simulator gets `interaction` events in one
/// per-entity sequence; nobody else may write the target.
#[tokio::test]
async fn cross_boundary_interaction_reaches_the_targets_owner() {
    let group = format!("xint-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim1, sim2, sim3) = (
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
    );
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim1, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim3, "cells": [{"x": 0, "y": 1}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim2, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(json!({"sim": sim1, "x": 95.0, "y": 50.0, "components": {"hp": 10}})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let target = e["id"].as_str().unwrap().to_string();
    // from the other node (sim2 sees the target as a ghost of cell (0,0))
    for dmg in [3, 4] {
        let (st, r) = http_json(
            &router2,
            "POST",
            &format!("/v1/server/worlds/{name}/interact"),
            None,
            true,
            Some(json!({"sim": sim2, "entity": target, "kind": "hit", "payload": {"damage": dmg}})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["delivered"], "forwarded", "{r}");
    }
    // from another simulator on the owner's node
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/interact"),
        None,
        true,
        Some(json!({"sim": sim3, "entity": target, "kind": "heal", "payload": {"amount": 1}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["delivered"], "owner", "{r}");
    let mut got: Vec<Value> = Vec::new();
    for _ in 0..40 {
        let (_, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim1}"),
            None,
            true,
            None,
        )
        .await;
        got.extend(
            ev["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e["type"] == "interaction"),
        );
        if got.len() >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(got.len(), 3, "{got:?}");
    let seqs: Vec<u64> = got.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3], "{got:?}");
    // the local call is synchronous and the forwarded ones cross the bus, so only the order of
    // events from one source is guaranteed: the two hits keep their order
    let mut kinds: Vec<&str> = got.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["heal", "hit", "hit"], "{got:?}");
    let hits: Vec<&Value> = got.iter().filter(|e| e["kind"] == "hit").collect();
    assert_eq!(hits[0]["payload"]["damage"], 3, "{got:?}");
    assert_eq!(hits[1]["payload"]["damage"], 4, "{got:?}");
    assert!(hits.iter().all(|e| e["from_sim"] == json!(sim2)), "{got:?}");
    let heal = got.iter().find(|e| e["kind"] == "heal").unwrap();
    assert_eq!(heal["from_sim"], json!(sim3), "{got:?}");
    // the owner is still the only writer: sim2 cannot apply to the target
    let (st, _) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(json!({"sim": sim2, "entity": target, "components": {"hp": 0}})),
    )
    .await;
    assert_ne!(st, 200);
}

/// A simulator that reports falling behind the world puts its cell into time dilation: tick
/// frames carry `time_scale` < 1 and the simulator gets a `time_scale` event; catching up
/// brings the cell back to real time.
#[tokio::test]
async fn time_dilation_follows_a_lagging_simulator() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let Some((addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0, "tidi_after_ticks": 5, "tidi_min": 0.1}})
                .to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let sim = uuid::Uuid::now_v7();
    let mut creq = format!("ws://{addr}/v1/server/worlds/{world}/stream?sim={sim}")
        .into_client_request()
        .unwrap();
    creq.headers_mut()
        .insert("x-musubi-server-key", "defaultkey".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(creq).await.unwrap();
    async fn next(ws: &mut Ws) -> Value {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    ws.send(Message::Text(
        json!({"op": "lease", "id": 1, "cells": [{"x": 0, "y": 0}]}).to_string(),
    ))
    .await
    .unwrap();
    // register so the controller knows this simulator; report tick 0 forever = falling behind
    let req = Request::post(format!("/v1/server/worlds/{world}/sims"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(json!({"sim": sim, "capacity": 4}).to_string()))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status().as_u16(),
        200
    );
    let t0 = std::time::Instant::now();
    let mut saw_event = false;
    let mut dilated: Option<f64> = None;
    let mut world_tick = 0u64;
    while dilated.is_none() || !saw_event {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "no dilation"
        );
        ws.send(Message::Text(
            json!({"op": "tick_done", "tick": 0}).to_string(),
        ))
        .await
        .unwrap();
        let v = next(&mut ws).await;
        if v["type"] != "tick" {
            continue;
        }
        world_tick = v["tick"].as_u64().unwrap_or(0);
        for c in v["cells"].as_array().cloned().unwrap_or_default() {
            if let Some(s) = c["time_scale"].as_f64() {
                assert!((0.1..1.0).contains(&s), "{v}");
                dilated = Some(s);
            }
        }
        if v["events"].as_array().is_some_and(|a| {
            a.iter()
                .any(|e| e["type"] == "time_scale" && e["scale"].as_f64().is_some_and(|s| s < 1.0))
        }) {
            saw_event = true;
        }
    }
    // catching up (reporting the current tick) returns the cell to real time
    let t0 = std::time::Instant::now();
    loop {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "still dilated"
        );
        ws.send(Message::Text(
            json!({"op": "tick_done", "tick": world_tick + 1000}).to_string(),
        ))
        .await
        .unwrap();
        let v = next(&mut ws).await;
        if v["type"] != "tick" {
            continue;
        }
        world_tick = v["tick"].as_u64().unwrap_or(world_tick);
        let cells = v["cells"].as_array().cloned().unwrap_or_default();
        if cells.iter().all(|c| c["time_scale"].is_null()) {
            break;
        }
    }
    let (st, dump) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/worlds/{world}/entities"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{dump}");
    assert_eq!(dump["leases"].as_array().unwrap().len(), 1, "{dump}");
}

/// `soft_cap` refuses spawns into a full cell (and tells the simulator), never handovers.
#[tokio::test]
async fn soft_cap_refuses_spawn_into_a_full_cell() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "soft_cap": 1}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn =
        |x: f64| json!({"sim": sim, "x": x, "y": 10.0, "components": {}, "persistent": false});
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(10.0)),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(20.0)),
    )
    .await;
    assert_ne!(st, 200, "{e}");
    assert!(e.to_string().contains("soft_cap"), "{e}");
    // another cell still has room
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(150.0)),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let (st, ev) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/events?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_full"),
        "{ev}"
    );
}

/// A spawn into a full cell with `queue: true` waits (202 + `queued`) and is admitted in order
/// once an entity leaves; a ticket older than `queue_ttl_ticks` expires with an event.
#[tokio::test]
async fn soft_cap_queue_admits_when_room_frees() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "soft_cap": 1, "queue_ttl_ticks": 20}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn = |x: f64, queue: bool| json!({"sim": sim, "x": x, "y": 10.0, "components": {"n": x}, "persistent": false, "queue": queue});
    let (st, a) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(10.0, false)),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let (st, q) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(20.0, true)),
    )
    .await;
    assert_eq!(st, 202, "{q}");
    assert_eq!(q["queued"], json!(true));
    assert_eq!(q["position"], json!(1));
    assert!(q["eta_ticks"].is_null(), "no departures seen yet: {q}");
    let ticket = q["ticket"].clone();
    let events = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let first = events(router.clone()).await;
    assert!(
        first
            .iter()
            .any(|e| e["type"] == "queued" && e["ticket"] == ticket),
        "{first:?}"
    );
    // nothing is admitted while the cell stays full
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let idle = events(router.clone()).await;
    assert!(
        !idle.iter().any(|e| e["type"] == "queue_admitted"),
        "{idle:?}"
    );
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/despawn"),
        None,
        true,
        Some(json!({"sim": sim, "entity": a["id"]})),
    )
    .await;
    assert_eq!(st, 200);
    let mut admitted = None;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Some(e) = events(router.clone())
            .await
            .into_iter()
            .find(|e| e["type"] == "queue_admitted")
        {
            admitted = Some(e);
            break;
        }
    }
    let admitted = admitted.expect("queued spawn admitted after the cell freed");
    assert_eq!(admitted["ticket"], ticket);
    assert_eq!(
        admitted["entity"]["components"][0]["value"],
        json!(20.0),
        "{admitted}"
    );
    let (st, view) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/view/0/0?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    assert!(
        view.to_string()
            .contains(admitted["entity"]["id"].as_str().unwrap()),
        "{view}"
    );
    // the cell is full again: a new ticket outlives queue_ttl_ticks and expires
    let (st, q2) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(30.0, true)),
    )
    .await;
    assert_eq!(st, 202, "{q2}");
    let mut expired = None;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Some(e) = events(router.clone())
            .await
            .into_iter()
            .find(|e| e["type"] == "queue_expired")
        {
            expired = Some(e);
            break;
        }
    }
    let expired = expired.expect("ticket expired after queue_ttl_ticks");
    assert_eq!(expired["ticket"], q2["ticket"]);
    assert_eq!(expired["reason"], json!("timeout"));
}

/// Plan 4.9 PES over HTTP: a spawn with `decay_ticks` disappears once nobody touches it (event
/// `decayed`); a leased cell nobody watches or writes to hibernates (`cell_hibernated`, lease
/// gone) keeping its persistent entity, and leasing it again finds that entity in place.
#[tokio::test]
async fn pes_decay_and_hibernation() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "hibernate_after_ticks": 10}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let lease = |router: axum::Router| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/lease"),
                None,
                true,
                Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
            )
            .await
        }
    };
    assert_eq!(lease(router.clone()).await.0, 200);
    let (st, keep) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {"kind": "chest"}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{keep}");
    let (st, litter) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 20.0, "y": 10.0, "components": {}, "persistent": false, "decay_ticks": 3})),
    )
    .await;
    assert_eq!(st, 200, "{litter}");
    let events = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let mut seen: Vec<Value> = Vec::new();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone()).await);
        if seen.iter().any(|e| e["type"] == "decayed") {
            break;
        }
    }
    let decayed = seen
        .iter()
        .find(|e| e["type"] == "decayed")
        .expect("litter decayed");
    assert_eq!(decayed["entity"], litter["id"], "{decayed}");
    // nobody watches or writes: the cell goes to sleep with the chest kept
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone()).await);
        if seen.iter().any(|e| e["type"] == "cell_hibernated") {
            break;
        }
    }
    let slept = seen
        .iter()
        .find(|e| e["type"] == "cell_hibernated")
        .unwrap_or_else(|| panic!("unwatched cell hibernated: {seen:?}"));
    assert_eq!(slept["persistent_kept"], json!(1), "{slept}");
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/apply"),
        None,
        true,
        Some(json!({"sim": sim, "entity": keep["id"], "components": {"kind": "chest"}})),
    )
    .await;
    assert_ne!(st, 200, "lease was revoked by hibernation: {e}");
    // the simulator leases the cell again: the chest is still there, the litter is not
    assert_eq!(lease(router.clone()).await.0, 200);
    let (st, view) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/view/0/0?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    let v = view.to_string();
    assert!(v.contains(keep["id"].as_str().unwrap()), "{view}");
    assert!(!v.contains(litter["id"].as_str().unwrap()), "{view}");
}

/// Plan 4.9 time machine: `POST /admin/worlds/{w}/snapshot` adds a history row; `GET
/// /admin/worlds/{w}/at?tick=T` rebuilds the persistent entities as of T from the newest history
/// snapshot at or before T plus the journal up to T; before the first snapshot it is a 404.
#[tokio::test]
async fn time_machine_rebuilds_world_at_tick() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn = |router: axum::Router, name: &'static str| {
        let world = world.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {"name": name}, "persistent": true})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let snapshot = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/admin/worlds/{world}/snapshot"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v["tick"].as_u64().unwrap()
        }
    };
    let at = |router: axum::Router, tick: u64| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "GET",
                &format!("/v1/admin/worlds/{world}/at?tick={tick}"),
                None,
                true,
                None,
            )
            .await
        }
    };
    let (st, v) = at(router.clone(), 0).await;
    assert_eq!(st, 404, "no history yet: {v}");
    let a = spawn(router.clone(), "a").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t1 = snapshot(router.clone()).await;
    // `at` is the state at the end of a tick: let the next mutations land in a later one
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/despawn"),
        None,
        true,
        Some(json!({"sim": sim, "entity": a})),
    )
    .await;
    assert_eq!(st, 200);
    let b = spawn(router.clone(), "b").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t2 = snapshot(router.clone()).await;
    // `at` is the state at the end of a tick: let the next mutations land in a later one
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(t2 > t1);
    let c = spawn(router.clone(), "c").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t3 = snapshot(router.clone()).await;
    assert!(t3 > t2 + 3);
    let ids = |v: &Value| -> Vec<String> {
        v["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    // as of the first snapshot: only a
    let (st, v) = at(router.clone(), t1).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), vec![a.clone()], "{v}");
    assert_eq!(v["history"]["oldest_tick"], json!(t1));
    assert_eq!(v["history"]["newest_tick"], json!(t3));
    // as of the second: a is gone, b is there
    let (st, v) = at(router.clone(), t2).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), vec![b.clone()], "{v}");
    // between the second and third snapshots: b from the snapshot, c replayed from the journal
    let (st, v) = at(router.clone(), t3 - 1).await;
    assert_eq!(st, 200, "{v}");
    let mut got = ids(&v);
    got.sort();
    let mut want = vec![b.clone(), c.clone()];
    want.sort();
    assert_eq!(got, want, "{v}");
    assert_eq!(v["from_snapshots"][0]["tick"], json!(t2), "{v}");
    assert!(v["replayed"].as_u64().unwrap() >= 1, "{v}");
    // before history began
    let (st, v) = at(router.clone(), t1 - 1).await;
    assert_eq!(st, 404, "{v}");
}

/// Lag compensation: the shard keeps `history_ticks` of positions per entity; `rewind` answers
/// where the target was at a past tick (and whether a claimed point matches), an `interaction`
/// with the actor's tick carries that answer to the owner, and history older than the window is
/// unknown rather than guessed.
#[tokio::test]
async fn interaction_rewinds_target_to_the_actors_tick() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "history_ticks": 8}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let target = e["id"].as_str().unwrap().to_string();
    let apply = |router: axum::Router, x: f64| {
        let (world, target) = (world.clone(), target.clone());
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/apply"),
                None,
                true,
                Some(json!({"sim": sim, "entity": target, "x": x, "y": 10.0, "components": {}})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
        }
    };
    let tick_now = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "GET",
                &format!("/v1/admin/worlds/{world}/entities"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v["tick"].as_u64().unwrap()
        }
    };
    let t_spawn = tick_now(router.clone()).await;
    // 12 ticks: the spawn sample falls out of an 8-tick history once a newer one is recorded
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    apply(router.clone(), 20.0).await;
    let t_mid = tick_now(router.clone()).await;
    assert!(t_mid > t_spawn + 8, "{t_mid} vs {t_spawn}");
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    apply(router.clone(), 30.0).await;
    let t_now = tick_now(router.clone()).await;
    assert!(t_now > t_mid + 1 && t_now < t_mid + 8, "{t_now} vs {t_mid}");
    let rewind = |router: axum::Router, tick: u64, x: f64| {
        let (world, target) = (world.clone(), target.clone());
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/rewind"),
                None,
                true,
                Some(json!({"entity": target, "tick": tick, "x": x, "y": 10.0, "tolerance": 0.5})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v
        }
    };
    // where it was then, not where it is now
    let v = rewind(router.clone(), t_mid, 20.0).await;
    assert_eq!(v["pos"]["x"], json!(20.0), "{v}");
    assert_eq!(v["hit"], json!(true), "{v}");
    let v = rewind(router.clone(), t_mid, 30.0).await;
    assert_eq!(v["hit"], json!(false), "{v}");
    let v = rewind(router.clone(), t_now, 30.0).await;
    assert_eq!(v["pos"]["x"], json!(30.0), "{v}");
    assert_eq!(v["hit"], json!(true), "{v}");
    // older than the kept history: unknown, not a guess
    let v = rewind(router.clone(), t_spawn, 10.0).await;
    assert!(v["pos"].is_null(), "{v}");
    assert!(v["hit"].is_null(), "{v}");
    // an interaction stamped with the actor's tick carries the rewind to the owner
    let shooter = uuid::Uuid::now_v7();
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/interact"),
        None,
        true,
        Some(
            json!({"sim": shooter, "entity": target, "kind": "hit", "payload": {"dmg": 5},
                    "tick": t_mid, "x": 20.0, "y": 10.0, "tolerance": 0.5}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["delivered"], json!("owner"));
    let (st, ev) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/events?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let hit = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "interaction")
        .unwrap_or_else(|| panic!("{ev}"));
    assert_eq!(hit["rewind"]["tick"], json!(t_mid), "{hit}");
    assert_eq!(hit["rewind"]["pos"]["x"], json!(20.0), "{hit}");
    assert_eq!(hit["rewind"]["hit"], json!(true), "{hit}");
}

/// Co-locate (plan 4.6): a simulator pins a partner entity from a neighbouring simulator's cell
/// into its own; the normal handover brings it over, it is authoritative over both while the
/// pin holds, and the partner goes back once the pin lapses.
#[tokio::test]
async fn colocate_pins_a_partner_into_the_callers_cell_and_returns_it() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "hysteresis": 5.0}})),
    )
    .await;
    assert_eq!(st, 200);
    let (s1, s2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (sim, x) in [(s1, 0), (s2, 1)] {
        let (st, _) = http_json(
            &router,
            "POST",
            &format!("/v1/server/worlds/{world}/lease"),
            None,
            true,
            Some(json!({"sim": sim, "cells": [{"x": x, "y": 0}]})),
        )
        .await;
        assert_eq!(st, 200);
    }
    let spawn = |router: axum::Router, sim: uuid::Uuid, x: f64| {
        let world = world.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": x, "y": 10.0, "components": {}, "persistent": false})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let a = spawn(router.clone(), s1, 10.0).await;
    let b = spawn(router.clone(), s2, 150.0).await;
    let apply = |router: axum::Router, sim: uuid::Uuid, id: String, x: f64| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/apply"),
                None,
                true,
                Some(json!({"sim": sim, "entity": id, "x": x, "y": 10.0, "components": {}})),
            )
            .await
            .0
        }
    };
    assert_ne!(
        apply(router.clone(), s1, b.clone(), 150.0).await,
        200,
        "s1 does not own b yet"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/colocate"),
        None,
        true,
        Some(json!({"sim": s1, "entity": a, "with": b, "ttl_ticks": 20})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["cell"], json!({"x": 0, "y": 0}));
    // the handover reaches s1 as a request it acks, exactly like a border crossing
    let events = |router: axum::Router, sim: uuid::Uuid| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let ack = |router: axum::Router, sim: uuid::Uuid, id: String| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/handover/ack"),
                None,
                true,
                Some(json!({"sim": sim, "entity": id})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
        }
    };
    let mut seen = Vec::new();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone(), s1).await);
        if seen
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b))
        {
            break;
        }
    }
    assert!(
        seen.iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b)),
        "{seen:?}"
    );
    ack(router.clone(), s1, b.clone()).await;
    let s2_events = events(router.clone(), s2).await;
    assert!(
        s2_events.iter().any(|e| e["type"] == "co_locate"),
        "{s2_events:?}"
    );
    // s1 now runs both; b keeps its position in the other cell
    assert_eq!(apply(router.clone(), s1, b.clone(), 151.0).await, 200);
    assert_ne!(apply(router.clone(), s2, b.clone(), 152.0).await, 200);
    // the pin lapses (20 ticks): b hands back to s2 by position
    let mut back = Vec::new();
    for _ in 0..80 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        back.extend(events(router.clone(), s2).await);
        if back
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b))
        {
            break;
        }
    }
    assert!(
        back.iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b)),
        "{back:?}"
    );
    ack(router.clone(), s2, b.clone()).await;
    assert_eq!(apply(router.clone(), s2, b.clone(), 153.0).await, 200);
    assert_ne!(apply(router.clone(), s1, b.clone(), 154.0).await, 200);
}

/// Multi-region: a world asked for a region with no live node there is placed on the creating
/// node (not left pending) and works as usual; the row remembers the region.
#[tokio::test]
async fn world_for_a_region_without_nodes_is_placed_here() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, v) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "region": "eu-nowhere", "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["name"], json!(world));
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let region: String = sqlx::query_scalar("select region from musubi.worlds where name = $1")
        .bind(&world)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(region, "eu-nowhere");
}

/// Cross-node co-locate: the partner lives in a cell hosted on another node; the request is
/// forwarded over the bus, that node pins and ships the entity through its regular cross-node
/// handover, and it lands in the caller's cell under the caller's authority.
#[tokio::test]
async fn colocate_pins_a_partner_from_another_node() {
    let group = format!("xcolo-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let spawn = |router: axum::Router, sim: uuid::Uuid, x: f64| {
        let name = name.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{name}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": x, "y": 50.0, "components": {}, "persistent": false})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let a = spawn(router1.clone(), sim_a, 50.0).await;
    let b = spawn(router2.clone(), sim_b, 150.0).await;
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/colocate"),
        None,
        true,
        Some(json!({"sim": sim_a, "entity": a, "with": b, "ttl_ticks": 600})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["forwarded"], json!(true), "{r}");
    assert_eq!(r["cell"], json!({"x": 0, "y": 0}));
    // the partner crosses nodes into the caller's cell
    let mut owned = Vec::new();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        owned = va["owned"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect();
        if owned.contains(&b) {
            break;
        }
    }
    if !owned.contains(&b) {
        let (_, d1) = http_json(
            &router1,
            "GET",
            &format!("/v1/admin/worlds/{name}/entities"),
            None,
            true,
            None,
        )
        .await;
        let (_, d2) = http_json(
            &router2,
            "GET",
            &format!("/v1/admin/worlds/{name}/entities"),
            None,
            true,
            None,
        )
        .await;
        let (_, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim_a}"),
            None,
            true,
            None,
        )
        .await;
        panic!("b not in caller's cell: {owned:?}\nnode1: {d1}\nnode2: {d2}\nsim_a events: {ev}");
    }
    assert!(owned.contains(&a));
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(json!({"sim": sim_a, "entity": b, "x": 151.0, "y": 50.0, "components": {}})),
    )
    .await;
    assert_eq!(st, 200, "caller owns the partner now: {v}");
}

/// Multi-region: a world asked for another region with a live node there is created pending
/// (202); a failover round on a node of another region leaves it alone while that region has a
/// live node, and a node of that region adopts it (the harness has no failover loop, so the
/// rounds are driven by hand).
#[tokio::test]
async fn world_asked_for_another_region_is_adopted_there() {
    let group = format!("xreg-{}", uuid::Uuid::now_v7().simple());
    let Some(state1) = build_state_in(true, &group, "eu").await else {
        return;
    };
    let Some(state2) = build_state_in(false, &group, "us").await else {
        return;
    };
    let (router1, router2) = (
        musubi_server::http::router(state1.clone()),
        musubi_server::http::router(state2.clone()),
    );
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let cluster_worlds = |router: axum::Router| async move {
        let (st, cw) =
            http_json(&router, "GET", "/v1/admin/cluster/worlds", None, true, None).await;
        assert_eq!(st, 200, "{cw}");
        cw
    };
    let cw = cluster_worlds(router2.clone()).await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, v) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": name, "region": "us", "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["pending"], json!(true));
    // (recover also sweeps up worlds left behind by earlier tests in the shared database, so
    // look for this world only)
    let taken = state1.worlds.recover(&[]).await.unwrap();
    assert!(
        !taken.contains(&name),
        "eu leaves a us world alone while a us node is alive: {taken:?}"
    );
    let taken = state2.worlds.recover(&[]).await.unwrap();
    assert!(taken.contains(&name), "us adopts it: {taken:?}");
    let cw = cluster_worlds(router1.clone()).await;
    let home = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == name)
        .and_then(|w| w["node_id"].as_str())
        .map(str::to_string);
    assert_eq!(
        home.as_deref(),
        Some(node2.as_str()),
        "adopted by the us node"
    );
    // and it serves from there
    let sim = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
}

/// Opt-in unreliable updates: a position-only move arrives as a sequenced frame, component
/// changes stay reliable, and the keyframe re-confirms unreliably delivered positions reliably.
#[tokio::test]
async fn world_unreliable_position_updates() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post(path)
                .header("x-musubi-server-key", "defaultkey")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, _) = post(
        "/v1/server/worlds".into(),
        json!({"name": world, "config": {"cell_size": 100.0, "keyframe_ticks": 5}}),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": sim, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);

    let f = "json";
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: true,
        },
    )
    .await;
    let (st, e) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": sim, "x": 50.0, "y": 50.0, "components": {"hp": 100}}),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            spawned, sequenced, ..
        } => {
            assert_eq!(spawned[0].id, eid);
            assert!(!sequenced, "spawns are reliable");
        }
        m => panic!("{m:?}"),
    }
    // position-only move → sequenced (unreliable) frame
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": sim, "entity": eid, "x": 60.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            updated, sequenced, ..
        } => {
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].x, 60.0);
            assert!(updated[0].components.is_empty());
            assert!(sequenced, "position-only update must be sequenced");
        }
        m => panic!("{m:?}"),
    }
    // the keyframe re-sends that position reliably
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no keyframe within 3s"
        );
        let m = tokio::time::timeout(std::time::Duration::from_secs(3), recv(&mut c))
            .await
            .expect("frame");
        if let ServerMessage::WorldDelta {
            updated,
            sequenced: false,
            ..
        } = m.msg
        {
            if updated.iter().any(|u| u.id == eid && u.x == 60.0) {
                break;
            }
        }
    }
    // a component change is reliable
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": sim, "entity": eid, "x": 60.0, "y": 50.0, "components": {"hp": 90}}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            updated, sequenced, ..
        } => {
            assert_eq!(updated[0].components[0].name, "hp");
            assert!(!sequenced);
        }
        m => panic!("{m:?}"),
    }
}

/// Drive a str0m client until an event the caller wants shows up (or 10 s pass).
async fn rtc_drive(
    rtc: &mut str0m::Rtc,
    sock: &tokio::net::UdpSocket,
    mut want: impl FnMut(&str0m::Event) -> bool,
) -> Option<str0m::Event> {
    let local = sock.local_addr().unwrap();
    let mut buf = vec![0u8; 2000];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                str0m::Output::Timeout(t) => break t,
                str0m::Output::Transmit(t) => {
                    sock.send_to(&t.contents, t.destination).await.unwrap();
                }
                str0m::Output::Event(e) => {
                    if want(&e) {
                        return Some(e);
                    }
                }
            }
        };
        if std::time::Instant::now() > deadline {
            return None;
        }
        let dur = timeout
            .saturating_duration_since(std::time::Instant::now())
            .max(std::time::Duration::from_millis(1));
        match tokio::time::timeout(dur, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                let contents = buf[..n].try_into().unwrap();
                rtc.handle_input(str0m::Input::Receive(
                    std::time::Instant::now(),
                    str0m::net::Receive {
                        proto: str0m::net::Protocol::Udp,
                        source,
                        destination: local,
                        contents,
                    },
                ))
                .unwrap();
            }
            _ => rtc
                .handle_input(str0m::Input::Timeout(std::time::Instant::now()))
                .unwrap(),
        }
    }
}

#[tokio::test]
async fn webrtc_datachannel_ping() {
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    let rtc_server =
        musubi_transport::webrtc::RtcServer::bind("127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    state.rtc = Some(rtc_server);
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;

    // browser side: a str0m peer with one data channel, offer over HTTP
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut rtc = str0m::Rtc::builder().build(std::time::Instant::now());
    rtc.add_local_candidate(str0m::Candidate::host(sock.local_addr().unwrap(), "udp").unwrap());
    let mut api = rtc.sdp_api();
    let cid = api.add_channel("reliable".into());
    let (offer, pending) = api.apply().unwrap();
    let req = Request::post("/v1/rtc/offer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&offer).unwrap()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["type"], "answer");
    let answer: str0m::change::SdpAnswer = serde_json::from_value(v).unwrap();
    rtc.sdp_api().accept_answer(pending, answer).unwrap();

    let opened = rtc_drive(
        &mut rtc,
        &sock,
        |e| matches!(e, str0m::Event::ChannelOpen(c, _) if *c == cid),
    )
    .await;
    assert!(opened.is_some(), "data channel never opened");
    let hello = Q::encode_datagram(&Frame::Text(
        json!({"token": ta, "format": "json"}).to_string(),
    ));
    rtc.channel(cid).unwrap().write(true, &hello).unwrap();
    let ping = Q::encode_datagram(&Frame::Text(
        serde_json::to_string(&Envelope {
            cid: Some("1".into()),
            msg: ClientMessage::Ping,
        })
        .unwrap(),
    ));
    rtc.channel(cid).unwrap().write(true, &ping).unwrap();
    let data = rtc_drive(&mut rtc, &sock, |e| {
        matches!(e, str0m::Event::ChannelData(_))
    })
    .await;
    let Some(str0m::Event::ChannelData(d)) = data else {
        panic!("no reply on the data channel")
    };
    let Frame::Text(t) = Q::decode_datagram(bytes::Bytes::from(d.data)).unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong), "{r:?}");
}

#[tokio::test]
async fn udp_transport_ping_and_bad_token() {
    use musubi_transport::{Connection, Frame};
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = musubi_server::realtime::udp::start(state, "127.0.0.1:0".parse().unwrap(), None)
        .await
        .unwrap();
    let (ta, _) = token(&router).await;

    let mut c = musubi_transport::udp::UdpClient::connect(addr, 1 << 20)
        .await
        .unwrap();
    c.send(Frame::Text(
        json!({"token": ta, "format": "json"}).to_string(),
    ))
    .await
    .unwrap();
    c.send(Frame::Text(
        serde_json::to_string(&Envelope {
            cid: Some("1".into()),
            msg: ClientMessage::Ping,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Frame::Text(t) = tokio::time::timeout(std::time::Duration::from_secs(5), c.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong), "{r:?}");

    let mut bad = musubi_transport::udp::UdpClient::connect(addr, 1 << 20)
        .await
        .unwrap();
    bad.send(Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = tokio::time::timeout(std::time::Duration::from_secs(5), bad.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}
