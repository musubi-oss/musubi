#include "MusubiSocket.h"
#include "MusubiModule.h"

FMusubiSocket::FMusubiSocket() = default;

FMusubiSocket::~FMusubiSocket()
{
	if (Transport)
	{
		Transport->OnConnected.RemoveAll(this);
		Transport->OnError.RemoveAll(this);
		Transport->OnClosed.RemoveAll(this);
		Transport->OnText.RemoveAll(this);
		Transport->OnBinary.RemoveAll(this);
		Transport.Reset();
	}
}

EMusubiTransport FMusubiSocket::GetTransportKind() const
{
	return Transport ? Transport->Kind() : EMusubiTransport::WebSocket;
}

FString FMusubiSocket::SocketUrl(const FString& BaseUrl, const FString& AccessToken, EMusubiEncoding Encoding)
{
	FMusubiTransportArgs Args;
	Args.BaseUrl = BaseUrl;
	Args.AccessToken = AccessToken;
	Args.Encoding = Encoding;
	return FMusubiWsTransport::SocketUrl(Args);
}

void FMusubiSocket::Connect(const FString& BaseUrl, const FString& AccessToken, const FConfig& InConfig)
{
	Config = InConfig;
	Base = BaseUrl;
	Token = AccessToken;
	bUserClosed = false;
	bEverConnected = false;
	bQuicFailed = false;
	Session.Reset();
	Backoff = Config.BackoffMinSec;
	Open();
}

void FMusubiSocket::SetAccessToken(const FString& AccessToken)
{
	Token = AccessToken;
}

void FMusubiSocket::Open()
{
	if (Transport)
	{
		Transport->OnConnected.RemoveAll(this);
		Transport->OnError.RemoveAll(this);
		Transport->OnClosed.RemoveAll(this);
		Transport->OnText.RemoveAll(this);
		Transport->OnBinary.RemoveAll(this);
		Transport.Reset();
	}
	EMusubiTransport Kind = FMusubiTransportRegistry::Resolve(Config.Transport);
	if (Kind == EMusubiTransport::Quic && Config.Transport == EMusubiTransport::Auto && bQuicFailed)
		Kind = EMusubiTransport::WebSocket;
	Transport = FMusubiTransportRegistry::Create(Kind);
	if (!Transport)
	{
		UE_LOG(LogMusubi, Error, TEXT("transport %d not available (is the MusubiQuic module enabled?)"), (int32)Kind);
		FailAllPending(TEXT("transport unavailable"));
		SetState(EMusubiSocketState::Closed);
		return;
	}
	Transport->OnConnected.AddRaw(this, &FMusubiSocket::HandleConnected);
	Transport->OnError.AddRaw(this, &FMusubiSocket::HandleError);
	Transport->OnClosed.AddRaw(this, &FMusubiSocket::HandleClosed);
	Transport->OnText.AddRaw(this, &FMusubiSocket::HandleText);
	Transport->OnBinary.AddRaw(this, &FMusubiSocket::HandleBinary);
	SetState(bEverConnected ? EMusubiSocketState::Reconnecting : EMusubiSocketState::Connecting);

	FMusubiTransportArgs Args;
	Args.BaseUrl = Base;
	Args.AccessToken = Token;
	Args.Encoding = Config.Encoding;
	Args.bResumable = Config.bResumable;
	// resume only across a reconnect of the same socket; a fresh Connect() starts a new session
	if (bEverConnected && Session.IsSet() && !Session->resume_token.IsEmpty()) Args.Resume = Session->resume_token;
	Args.QuicPort = Config.QuicPort;
	Args.bInsecure = Config.bQuicInsecure;
	Args.MaxFrameBytes = Config.MaxFrameBytes;
	Transport->Connect(Args);
}

void FMusubiSocket::Close(int32 Code, const FString& Reason)
{
	bUserClosed = true;
	ReconnectAt = 0.0;
	if (Transport && Transport->IsConnected())
	{
		Transport->Close(Code, Reason);
	}
	else
	{
		FailAllPending(TEXT("socket closed"));
		SetState(EMusubiSocketState::Closed);
	}
}

void FMusubiSocket::SetState(EMusubiSocketState NewState)
{
	if (State == NewState) return;
	State = NewState;
	OnStateChanged.Broadcast(State);
}

void FMusubiSocket::HandleConnected()
{
	const bool bReconnect = bEverConnected;
	bEverConnected = true;
	Backoff = Config.BackoffMinSec;
	NextPingAt = Now + Config.PingIntervalSec;
	SetState(EMusubiSocketState::Open);
	if (bReconnect)
	{
		Stats.Reconnects++;
		OnResubscribe.Broadcast();
	}
}

void FMusubiSocket::HandleError(const FString& Error)
{
	UE_LOG(LogMusubi, Warning, TEXT("socket error (%s): %s"),
		GetTransportKind() == EMusubiTransport::Quic ? TEXT("quic") : TEXT("ws"), *Error);
	if (!bEverConnected && Config.Transport == EMusubiTransport::Auto && GetTransportKind() == EMusubiTransport::Quic && !bQuicFailed)
	{
		// Auto: QUIC never handshook (UDP blocked, wrong port, cert) → retry at once over WebSocket.
		bQuicFailed = true;
		UE_LOG(LogMusubi, Log, TEXT("quic unavailable, falling back to websocket"));
		ReconnectAt = Now;
		SetState(EMusubiSocketState::Reconnecting);
		return;
	}
	FailAllPending(Error);
	if (!bUserClosed && Config.bAutoReconnect) ScheduleReconnect();
	else SetState(EMusubiSocketState::Closed);
}

void FMusubiSocket::HandleClosed(int32 Code, const FString& Reason, bool bWasClean)
{
	UE_LOG(LogMusubi, Log, TEXT("socket closed %d %s%s"), Code, *Reason, bWasClean ? TEXT("") : TEXT(" (unclean)"));
	FailAllPending(TEXT("socket closed"));
	OnClosed.Broadcast(Code, Reason);
	if (!bUserClosed && Config.bAutoReconnect) ScheduleReconnect();
	else SetState(EMusubiSocketState::Closed);
}

void FMusubiSocket::ScheduleReconnect()
{
	const double Jitter = 1.0 + (FMath::FRand() * 0.4 - 0.2);
	ReconnectAt = Now + Backoff * Jitter;
	Backoff = FMath::Min(Backoff * 2.0, Config.BackoffMaxSec);
	SetState(EMusubiSocketState::Reconnecting);
}

void FMusubiSocket::HandleText(const FString& Text)
{
	Stats.FramesIn++;
	Stats.BytesIn += Text.Len();
	FMusubiServerEnvelope Env;
	if (!Env.FromJsonText(Text))
	{
		Stats.DecodeErrors++;
		UE_LOG(LogMusubi, Warning, TEXT("undecodable frame: %s"), *Text.Left(200));
		return;
	}
	Dispatch(Env);
}

void FMusubiSocket::HandleBinary(const uint8* Data, int32 Size)
{
	Stats.FramesIn++;
	Stats.BytesIn += Size;
	FMusubiServerEnvelope Env;
	const bool bOk = Env.FromPostcard(Data, Size);
	if (!bOk)
	{
		Stats.DecodeErrors++;
		UE_LOG(LogMusubi, Warning, TEXT("undecodable binary frame (%d bytes, head %s)"), Size, *BytesToHex(Data, FMath::Min(Size, 24)));
		return;
	}
	Dispatch(Env);
}

void FMusubiSocket::Dispatch(const FMusubiServerEnvelope& Env)
{
	switch (Env.Msg.Tag)
	{
	case EMusubiServerMessageTag::Session:
		Session = Env.Msg.Session;
		break;
	case EMusubiServerMessageTag::Pong:
		if (LastRttSentAt > 0.0)
		{
			const double Sample = (Now - LastRttSentAt) * 1000.0;
			Stats.RttMs = Stats.RttMs <= 0.0 ? Sample : Stats.RttMs * 0.8 + Sample * 0.2;
			LastRttSentAt = 0.0;
		}
		break;
	default:
		break;
	}

	if (Env.Cid.IsSet())
	{
		FPending P;
		if (Pending.RemoveAndCopyValue(Env.Cid.GetValue(), P))
		{
			P.OnReply.ExecuteIfBound(Env.Msg.Tag != EMusubiServerMessageTag::Error, Env);
			// A correlated reply is also an event (e.g. `match` after match_join) — fall through.
		}
	}
	OnMessage.Broadcast(Env);
	if (FMusubiServerMessageEvent* Ev = TagEvents.Find(Env.Msg.Tag))
	{
		Ev->Broadcast(Env);
	}
}

bool FMusubiSocket::Transmit(const FMusubiClientEnvelope& Env)
{
	if (!IsConnected() || !Transport) return false;
	if (Config.Encoding == EMusubiEncoding::Postcard)
	{
		TArray<uint8> Bytes = Env.ToPostcard();
		if (Bytes.Num() == 0) return false;
		Stats.BytesOut += Bytes.Num();
		if (!Transport->SendBinary(Bytes.GetData(), Bytes.Num(), false)) return false;
	}
	else
	{
		const FString Text = Env.ToJsonText();
		Stats.BytesOut += Text.Len();
		if (!Transport->SendText(Text)) return false;
	}
	Stats.FramesOut++;
	return true;
}

bool FMusubiSocket::Send(const FMusubiClientMessage& Msg)
{
	FMusubiClientEnvelope Env;
	Env.Msg = Msg;
	return Transmit(Env);
}

void FMusubiSocket::FailPending(FPending&& P, const FString& Why, EMusubiCodeTag Code)
{
	FMusubiServerEnvelope Env;
	Env.Msg.Tag = EMusubiServerMessageTag::Error;
	FMusubiServerMessageError E;
	E.code.Tag = Code;
	E.message = Why;
	Env.Msg.Error = MoveTemp(E);
	P.OnReply.ExecuteIfBound(false, Env);
}

FString FMusubiSocket::Request(const FMusubiClientMessage& Msg, FMusubiReply OnReply)
{
	if (!IsConnected())
	{
		FPending P;
		P.OnReply = MoveTemp(OnReply);
		FailPending(MoveTemp(P), TEXT("socket not connected"), EMusubiCodeTag::Unavailable);
		return FString();
	}
	if (Pending.Num() >= Config.MaxInflight)
	{
		FPending P;
		P.OnReply = MoveTemp(OnReply);
		FailPending(MoveTemp(P), TEXT("too many requests in flight"), EMusubiCodeTag::ResourceExhausted);
		return FString();
	}
	FMusubiClientEnvelope Env;
	Env.Cid = FString::Printf(TEXT("%llu"), NextCid++);
	Env.Msg = Msg;
	FPending P;
	P.OnReply = MoveTemp(OnReply);
	P.Deadline = Now + Config.ReplyTimeoutSec;
	const FString Cid = Env.Cid.GetValue();
	if (!Transmit(Env))
	{
		FailPending(MoveTemp(P), TEXT("send failed"), EMusubiCodeTag::Unavailable);
		return FString();
	}
	Pending.Add(Cid, MoveTemp(P));
	return Cid;
}

void FMusubiSocket::FailAllPending(const FString& Why)
{
	TMap<FString, FPending> Drop = MoveTemp(Pending);
	Pending.Reset();
	for (auto& It : Drop)
	{
		FailPending(MoveTemp(It.Value), Why, EMusubiCodeTag::Unavailable);
	}
}

void FMusubiSocket::Tick(double DeltaSeconds)
{
	Now += DeltaSeconds;
	if (Transport) Transport->Tick(DeltaSeconds);

	if (State == EMusubiSocketState::Reconnecting && ReconnectAt > 0.0 && Now >= ReconnectAt)
	{
		ReconnectAt = 0.0;
		Open();
	}

	if (!IsConnected()) return;

	if (Pending.Num() > 0)
	{
		TArray<FString> Expired;
		for (const auto& It : Pending)
		{
			if (Now >= It.Value.Deadline) Expired.Add(It.Key);
		}
		for (const FString& Cid : Expired)
		{
			FPending P;
			Pending.RemoveAndCopyValue(Cid, P);
			Stats.Timeouts++;
			FailPending(MoveTemp(P), TEXT("reply timeout"), EMusubiCodeTag::Unavailable);
		}
	}

	if (Now >= NextPingAt)
	{
		NextPingAt = Now + Config.PingIntervalSec;
		FMusubiClientMessage Ping;
		Ping.Tag = EMusubiClientMessageTag::Ping;
		LastRttSentAt = Now;
		Send(Ping);
	}
}
