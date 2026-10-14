use std::sync::Arc;
use std::time::Duration;

use musubi_chat::{Channel, ChatService};
use musubi_core::{AccountId, Error, GroupId};
use musubi_matchmaker::{AddTicket, Matchmaker};
use musubi_proto::realtime::{ChannelKind, MatchmakerUser};
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage, UserPresence};
use musubi_transport::{Connection, Frame, Limits};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::hub::SessionHandle;
use super::Hub;

pub struct ConnectParams {
    pub presence: UserPresence,
    pub encoding: Encoding,
    pub limits: Limits,
    pub chat: ChatService,
    pub matchmaker: Arc<Matchmaker>,
}

/// Drive one realtime connection until it closes. Registers with the hub on entry and
/// always unregisters on exit.
pub async fn serve(hub: Arc<Hub>, mut conn: Box<dyn Connection>, params: ConnectParams) {
    let ConnectParams {
        presence,
        encoding,
        limits,
        chat,
        matchmaker,
    } = params;
    let sid = presence.session_id;
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(256);
    hub.join(SessionHandle {
        presence: presence.clone(),
        tx: tx.clone(),
    });
    info!(session = %sid, account = %presence.account_id, transport = conn.kind(), "realtime connected");

    let ctx = Ctx {
        hub: &hub,
        chat: &chat,
        matchmaker: &matchmaker,
        presence: &presence,
    };
    let result = run_loop(&ctx, conn.as_mut(), encoding, limits, tx, &mut rx).await;
    matchmaker.remove_session(sid);
    hub.leave(sid);
    conn.close().await;
    match result {
        Ok(()) => info!(session = %sid, "realtime disconnected"),
        Err(e) => debug!(session = %sid, error = %e, "realtime closed with error"),
    }
}

struct Ctx<'a> {
    hub: &'a Hub,
    chat: &'a ChatService,
    matchmaker: &'a Matchmaker,
    presence: &'a UserPresence,
}

async fn run_loop(
    ctx: &Ctx<'_>,
    conn: &mut dyn Connection,
    encoding: Encoding,
    limits: Limits,
    tx: mpsc::Sender<ServerMessage>,
    rx: &mut mpsc::Receiver<ServerMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sid = ctx.presence.session_id;
    let mut idle = tokio::time::interval(limits.idle_timeout);
    idle.reset();
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(msg) = out else { return Ok(()) };
                let bytes = encoding.encode(&Envelope { cid: None, msg })?;
                conn.send(Frame::encoded(encoding, bytes)).await?;
            }
            inc = conn.recv() => {
                let frame = inc?;
                idle.reset();
                let Some(payload) = frame.payload() else { return Ok(()) };
                if payload.len() > limits.max_frame_bytes {
                    send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument("frame too large".into()))).await?;
                    continue;
                }
                let env: Envelope<ClientMessage> = match encoding.decode(payload) {
                    Ok(e) => e,
                    Err(e) => {
                        send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument(format!("bad frame: {e}")))).await?;
                        continue;
                    }
                };
                if let Some(reply) = handle(ctx, env.msg).await {
                    let bytes = encoding.encode(&Envelope { cid: env.cid, msg: reply })?;
                    conn.send(Frame::encoded(encoding, bytes)).await?;
                }
            }
            _ = idle.tick() => {
                warn!(session = %sid, "idle timeout");
                let _ = tx.try_send(ServerMessage::error(&Error::Unavailable("idle timeout".into())));
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(());
            }
        }
    }
}

async fn send(
    conn: &mut dyn Connection,
    enc: Encoding,
    cid: Option<String>,
    msg: ServerMessage,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bytes = enc.encode(&Envelope { cid, msg })?;
    conn.send(Frame::encoded(enc, bytes)).await?;
    Ok(())
}

async fn handle(ctx: &Ctx<'_>, msg: ClientMessage) -> Option<ServerMessage> {
    let hub = ctx.hub;
    let sid = ctx.presence.session_id;
    match msg {
        ClientMessage::ChannelJoin {
            target,
            kind,
            persistence: _,
            hidden,
        } => Some(
            channel_join(ctx, &target, kind, hidden)
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelLeave { channel_id } => {
            hub.leave_channel(&channel_id, sid);
            None
        }
        ClientMessage::ChannelMessageSend {
            channel_id,
            content,
        } => Some(
            channel_send(ctx, &channel_id, content)
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelMessageUpdate {
            channel_id,
            message_id,
            content,
        } => Some(
            channel_edit(ctx, &channel_id, &message_id, Some(content))
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelMessageRemove {
            channel_id,
            message_id,
        } => Some(
            channel_edit(ctx, &channel_id, &message_id, None)
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::MatchmakerAdd {
            min_count,
            max_count,
            query,
            string_properties,
            numeric_properties,
            count_multiple,
        } => {
            let me = MatchmakerUser {
                presence: ctx.presence.clone(),
                string_properties,
                numeric_properties,
            };
            let res = ctx.matchmaker.add(AddTicket {
                party_id: None,
                members: vec![me],
                query,
                min_count: min_count as usize,
                max_count: max_count as usize,
                count_multiple: count_multiple as usize,
            });
            match res {
                Ok(ticket) => {
                    super::matchmaking::process_and_deliver(hub, ctx.matchmaker);
                    Some(ServerMessage::MatchmakerTicket { ticket })
                }
                Err(e) => Some(ServerMessage::error(&e)),
            }
        }
        ClientMessage::MatchmakerRemove { ticket } => match ctx.matchmaker.remove(sid, &ticket) {
            Ok(()) => None,
            Err(e) => Some(ServerMessage::error(&e)),
        },
        ClientMessage::Ping => Some(ServerMessage::Pong),
        ClientMessage::StatusUpdate { status } => {
            if status.as_ref().is_some_and(|s| s.chars().count() > 128) {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "status must be ≤ 128 chars".into(),
                )));
            }
            hub.set_status(sid, status);
            None
        }
        ClientMessage::StatusFollow { account_ids } => {
            if account_ids.len() > 1000 {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "follow at most 1000 users".into(),
                )));
            }
            Some(ServerMessage::Status {
                presences: hub.follow(sid, &account_ids),
            })
        }
        ClientMessage::StatusUnfollow { account_ids } => {
            hub.unfollow(sid, &account_ids);
            None
        }
        ClientMessage::Rpc { id, .. } => Some(ServerMessage::error(&Error::NotFound(format!(
            "rpc {id}: runtime not loaded"
        )))),
    }
}

fn resolve_channel(me: AccountId, target: &str, kind: ChannelKind) -> musubi_core::Result<Channel> {
    Ok(match kind {
        ChannelKind::Room => {
            musubi_chat::validate_room(target)?;
            Channel::Room(target.to_string())
        }
        ChannelKind::Direct => {
            let other: AccountId = target
                .parse()
                .map_err(|_| Error::InvalidArgument("bad user id".into()))?;
            Channel::direct(me, other)
        }
        ChannelKind::Group => {
            let g: GroupId = target
                .parse()
                .map_err(|_| Error::InvalidArgument("bad group id".into()))?;
            Channel::Group(g)
        }
    })
}

async fn channel_join(
    ctx: &Ctx<'_>,
    target: &str,
    kind: ChannelKind,
    hidden: bool,
) -> musubi_core::Result<ServerMessage> {
    let me = ctx.presence.account_id;
    let channel = resolve_channel(me, target, kind)?;
    ctx.chat.authorize(me, &channel).await?;
    let id = channel.id();
    let presences = ctx.hub.join_channel(&id, ctx.presence.session_id, hidden);
    Ok(ServerMessage::Channel {
        id,
        kind,
        presences,
        me: ctx.presence.clone(),
    })
}

fn joined_channel(ctx: &Ctx<'_>, channel_id: &str) -> musubi_core::Result<Channel> {
    let channel = Channel::parse(channel_id)?;
    if !ctx.hub.in_channel(channel_id, ctx.presence.session_id) {
        return Err(Error::PermissionDenied("join the channel first".into()));
    }
    Ok(channel)
}

async fn channel_send(
    ctx: &Ctx<'_>,
    channel_id: &str,
    content: serde_json::Value,
) -> musubi_core::Result<ServerMessage> {
    let channel = joined_channel(ctx, channel_id)?;
    let persistent = !matches!(channel, Channel::Room(_));
    let msg = ctx
        .chat
        .append(
            &channel,
            ctx.presence.account_id,
            &ctx.presence.username,
            content,
            persistent,
        )
        .await?;
    let ack = ack_of(&msg);
    ctx.hub
        .broadcast_channel(channel_id, ServerMessage::ChannelMessage(msg));
    Ok(ack)
}

async fn channel_edit(
    ctx: &Ctx<'_>,
    channel_id: &str,
    message_id: &str,
    content: Option<serde_json::Value>,
) -> musubi_core::Result<ServerMessage> {
    let channel = joined_channel(ctx, channel_id)?;
    let me = ctx.presence.account_id;
    let msg = match content {
        Some(c) => ctx.chat.update(&channel, me, message_id, c).await?,
        None => ctx.chat.remove(&channel, me, message_id).await?,
    };
    let ack = ack_of(&msg);
    ctx.hub
        .broadcast_channel(channel_id, ServerMessage::ChannelMessage(msg));
    Ok(ack)
}

fn ack_of(m: &musubi_proto::realtime::ChannelMessage) -> ServerMessage {
    ServerMessage::ChannelMessageAck {
        channel_id: m.channel_id.clone(),
        message_id: m.message_id.clone(),
        code: m.code,
        create_time: m.create_time,
        update_time: m.update_time,
        persistent: m.persistent,
    }
}
