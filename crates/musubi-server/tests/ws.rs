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
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let hub = Arc::new(Hub::new());
    let state = AppState {
        auth: AuthService::new(db.clone(), &cfg.session).unwrap(),
        storage: StorageService::new(db.clone()),
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            hub.clone(),
        ),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        matchmaker: Arc::new(musubi_matchmaker::Matchmaker::default()),
        hub,
        cfg: Arc::new(cfg),
        db,
        started_at: std::time::Instant::now(),
    };
    let router = musubi_server::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    Some((format!("127.0.0.1:{}", addr.port()), router))
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
