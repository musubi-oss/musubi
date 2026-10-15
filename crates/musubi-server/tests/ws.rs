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
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    cfg.database.max_connections = 4;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let hub = Arc::new(Hub::new());
    let runtime = test_runtime();
    let cluster = test_cluster(&db, group).await;
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
    fanout.set_worlds(worlds.clone());
    worlds.clone().spawn_ticker(fanout.clone());
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
        apple_iap: None,
        google_iap: None,
        steam_iap: None,
        runtime: runtime.clone(),
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        cfg: Arc::new(cfg),
        events: Arc::new(musubi_server::events::Events::new(db.clone())),
        banned: Arc::new(musubi_server::bans::Bans::default()),
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

async fn test_cluster(db: &sqlx::PgPool, group: &str) -> Option<Arc<musubi_cluster::Cluster>> {
    let c = Arc::new(
        musubi_cluster::Cluster::new(
            db.clone(),
            "test-node",
            "local",
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
        },
    )
    .await;

    let (st, e) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": s1, "x": 50.0, "y": 50.0, "components": {"hp": 100}}),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let eid = e["id"].as_str().unwrap().to_string();
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
            entity: eid.clone(),
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
            .any(|e| e["type"] == "input" && e["entity"] == eid),
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
            .any(|e| e["type"] == "handover_request" && e["entity"] == eid)
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
                entity: e.id.clone(),
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
            entity: e.id.clone(),
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
            .any(|ev| ev["type"] == "input" && ev["entity"] == e.id)
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
    migrate_once(&db).await;
    let (n1, n2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone()));
    let name = format!("w-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
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

    let r2 = musubi_server::realtime::WorldRegistry::new(n2, Some(db.clone()));
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
    assert!(s.contains(&keep.id), "{s}");
    assert!(s.contains("\"hp\""), "{s}");
    // n2 is a live node now, so a third registry has nothing left to claim
    sqlx::query("insert into musubi.nodes (id, name, cluster_group) values ($1, 'n2', $2)")
        .bind(n2)
        .bind(name.clone())
        .execute(&db)
        .await
        .unwrap();
    let r3 = musubi_server::realtime::WorldRegistry::new(uuid::Uuid::now_v7(), Some(db.clone()));
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
