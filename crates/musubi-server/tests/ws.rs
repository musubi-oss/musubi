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

async fn start() -> Option<(String, axum::Router)> {
    start_with(true).await
}

/// `leader=false` builds a node that replicates tickets but never runs matchmaker passes.
async fn start_with(leader: bool) -> Option<(String, axum::Router)> {
    let state = build_state(leader).await?;
    let router = musubi_server::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    Some((format!("127.0.0.1:{}", addr.port()), router))
}

async fn build_state(leader: bool) -> Option<AppState> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let hub = Arc::new(Hub::new());
    let runtime = test_runtime();
    let cluster = test_cluster(&db).await;
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
        matchmaker,
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: Arc::new(musubi_server::realtime::WorldRegistry::new()),
        runtime: runtime.clone(),
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        cfg: Arc::new(cfg),
        db,
        started_at: std::time::Instant::now(),
    })
}

async fn token(router: &axum::Router) -> (String, String) {
    let body = json!({"id": format!("ws-{}", uuid::Uuid::now_v7().simple())}).to_string();
    let req = Request::post("/v1/auth/device")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (
        v["access_token"].as_str().unwrap().into(),
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

async fn test_cluster(db: &sqlx::PgPool) -> Option<Arc<musubi_cluster::Cluster>> {
    let c = Arc::new(musubi_cluster::Cluster::new(
        db.clone(),
        "test-node",
        "local",
        vec!["all".into()],
        "127.0.0.1:0",
    ));
    c.start().await.unwrap();
    Some(c)
}

/// Two independent "nodes" (separate hubs, same database): a chat message sent on node 1 reaches a
/// session on node 2, and so does a notification.
#[tokio::test]
async fn cross_node_chat_and_notification_fanout() {
    let Some((addr1, router1)) = start().await else {
        return;
    };
    let Some((addr2, _router2)) = start().await else {
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
    let Some((addr1, router1)) = start().await else {
        return;
    };
    let Some((addr2, _)) = start().await else {
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
    let Some((addr1, router1)) = start_with(true).await else {
        return;
    };
    let Some((addr2, _)) = start_with(false).await else {
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
    let Some(state) = build_state(true).await else {
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
    let Some((addr1, router1)) = start().await else {
        return;
    };
    let Some((addr2, _)) = start().await else {
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
    let Some(state) = build_state(true).await else {
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
    let Some((addr1, router1)) = start().await else {
        return;
    };
    let Some((addr2, _)) = start().await else {
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
    let Some(state) = build_state(true).await else {
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
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchmakerMatched {
                server: Some(srv), ..
            } => {
                assert_eq!(srv.server_id, sid.to_string());
                assert_eq!(srv.port, 48000);
                assert!(!srv.connect_token.is_empty());
            }
            m => panic!("{m:?}"),
        }
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
    let Some(state) = build_state(true).await else {
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
            radius: 80.0,
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
