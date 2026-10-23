// Realtime socket: typed `Envelope{cid, msg}` frames over an IMusubiTransport (WebSocket `/ws`
// in this module, QUIC via the optional `MusubiQuic` module). Every ClientMessage variant goes out through `Send`/`Request`; every
// ServerMessage variant arrives on `OnMessage` (and `OnTag(tag)` for one-tag listeners).
// Requests carry a correlation id; the first reply with the same cid resolves them (an
// `error` body resolves with bOk = false; no reply within ReplyTimeout → bOk = false, Timeout).
// Reconnects with exponential backoff and replays the subscription state (`Resubscribe`).
#pragma once

#include "CoreMinimal.h"
#include "MusubiTransport.h"
#include "MusubiEnvelope.h"

enum class EMusubiSocketState : uint8
{
	Closed,
	Connecting,
	Open,
	Reconnecting,
};

struct MUSUBI_API FMusubiSocketStats
{
	uint64 FramesIn = 0;
	uint64 FramesOut = 0;
	uint64 BytesIn = 0;
	uint64 BytesOut = 0;
	uint64 DecodeErrors = 0;
	uint64 Timeouts = 0;
	uint32 Reconnects = 0;
	/** Last `rtt` round trip in milliseconds (EWMA, α = 0.2). */
	double RttMs = 0.0;
};

/** Reply to a request: bOk = false when the node answered `error`, the request timed out, or the socket dropped. */
DECLARE_DELEGATE_TwoParams(FMusubiReply, bool /*bOk*/, const FMusubiServerEnvelope& /*Reply; Msg.Tag == Error when !bOk*/);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiServerMessageEvent, const FMusubiServerEnvelope&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiSocketStateEvent, EMusubiSocketState);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiSocketClosedEvent, int32 /*Code*/, const FString& /*Reason*/);
/** Fired after a reconnect handshake; the owner re-sends subscriptions (worlds, channels, follows). */
DECLARE_MULTICAST_DELEGATE(FMusubiResubscribeEvent);

class MUSUBI_API FMusubiSocket : public TSharedFromThis<FMusubiSocket>
{
public:
	struct FConfig
	{
		EMusubiEncoding Encoding = EMusubiEncoding::Json;
		double ReplyTimeoutSec = 8.0;
		double PingIntervalSec = 15.0;
		double BackoffMinSec = 0.25;
		double BackoffMaxSec = 8.0;
		int32 MaxInflight = 64;
		/** Reconnect automatically after an unexpected close. */
		bool bAutoReconnect = true;
		/** Frames larger than this are refused before decode. */
		int32 MaxFrameBytes = 4 * 1024 * 1024;
		/** Auto = QUIC when `MusubiQuic` is loaded (falls back to WebSocket if the QUIC connect fails), else WebSocket. */
		EMusubiTransport Transport = EMusubiTransport::Auto;
		/** QUIC UDP port (the node's `quic.listen`); 0 = same port as the API base. */
		int32 QuicPort = 0;
		/** QUIC: accept self-signed node certificates (development). */
		bool bQuicInsecure = false;
		/** Ask the node for a resume token; reconnects then resume the parked session instead of rebuilding it. */
		bool bResumable = true;
	};

	FMusubiSocket();
	~FMusubiSocket();

	/** `BaseUrl` is the http(s) API base; the socket URL and `?format=` are derived. */
	void Connect(const FString& BaseUrl, const FString& AccessToken, const FConfig& InConfig);
	/** Replace the access token used by the next (re)connect, e.g. after a refresh. */
	void SetAccessToken(const FString& AccessToken);
	void Close(int32 Code = 1000, const FString& Reason = TEXT("client closed"));
	bool IsConnected() const { return State == EMusubiSocketState::Open; }
	EMusubiSocketState GetState() const { return State; }
	const FMusubiSocketStats& GetStats() const { return Stats; }
	const FConfig& GetConfig() const { return Config; }
	/** Session id / resume token from the node's `session` message. */
	const TOptional<FMusubiServerMessageSession>& GetSession() const { return Session; }

	/** Fire-and-forget (no cid). Returns false when not connected (frame dropped and counted). */
	bool Send(const FMusubiClientMessage& Msg);
	/** Correlated request; returns the cid (empty when not connected — OnReply fires immediately with bOk = false). */
	FString Request(const FMusubiClientMessage& Msg, FMusubiReply OnReply);

	/** Pump timers (reply timeouts, pings, reconnect). Called by the subsystem each frame. */
	void Tick(double DeltaSeconds);

	FMusubiServerMessageEvent OnMessage;
	FMusubiSocketStateEvent OnStateChanged;
	FMusubiSocketClosedEvent OnClosed;
	FMusubiResubscribeEvent OnResubscribe;

	/** Per-tag listeners: `OnTag(EMusubiServerMessageTag::WorldDelta).AddRaw(...)`. */
	FMusubiServerMessageEvent& OnTag(EMusubiServerMessageTag Tag) { return TagEvents.FindOrAdd(Tag); }

	/** Transport actually in use (Auto resolved); WebSocket when not connected yet. */
	EMusubiTransport GetTransportKind() const;

	static FString SocketUrl(const FString& BaseUrl, const FString& AccessToken, EMusubiEncoding Encoding);

private:
	struct FPending
	{
		FMusubiReply OnReply;
		double Deadline = 0.0;
	};

	void Open();
	void SetState(EMusubiSocketState NewState);
	void HandleConnected();
	void HandleError(const FString& Error);
	void HandleClosed(int32 Code, const FString& Reason, bool bWasClean);
	void HandleText(const FString& Text);
	void HandleBinary(const uint8* Data, int32 Size);
	void Dispatch(const FMusubiServerEnvelope& Env);
	void FailAllPending(const FString& Why);
	void FailPending(FPending&& P, const FString& Why, EMusubiCodeTag Code);
	bool Transmit(const FMusubiClientEnvelope& Env);
	void ScheduleReconnect();

	FConfig Config;
	FString Base;
	FString Token;
	TUniquePtr<IMusubiTransport> Transport;
	/** Auto: QUIC failed before the first handshake — stay on WebSocket for this socket's lifetime. */
	bool bQuicFailed = false;
	EMusubiSocketState State = EMusubiSocketState::Closed;
	TMap<FString, FPending> Pending;
	TMap<EMusubiServerMessageTag, FMusubiServerMessageEvent> TagEvents;
	TOptional<FMusubiServerMessageSession> Session;
	FMusubiSocketStats Stats;
	uint64 NextCid = 1;
	double Now = 0.0;
	double NextPingAt = 0.0;
	double ReconnectAt = 0.0;
	double Backoff = 0.0;
	double LastRttSentAt = 0.0;
	bool bUserClosed = false;
	bool bEverConnected = false;
};
