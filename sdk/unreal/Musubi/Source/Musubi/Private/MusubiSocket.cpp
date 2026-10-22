#include "MusubiSocket.h"
#include "MusubiModule.h"
#include "WebSocketsModule.h"
#include "GenericPlatform/GenericPlatformHttp.h"

FMusubiSocket::FMusubiSocket() = default;

FMusubiSocket::~FMusubiSocket()
{
	if (Ws.IsValid())
	{
		Ws->OnConnected().RemoveAll(this);
		Ws->OnConnectionError().RemoveAll(this);
		Ws->OnClosed().RemoveAll(this);
		Ws->OnMessage().RemoveAll(this);
		Ws->OnRawMessage().RemoveAll(this);
		if (Ws->IsConnected()) Ws->Close();
	}
}

FString FMusubiSocket::SocketUrl(const FString& BaseUrl, const FString& AccessToken, EMusubiEncoding Encoding)
{
	FString Url = BaseUrl;
	Url.RemoveFromEnd(TEXT("/"));
	if (Url.StartsWith(TEXT("https://"))) Url = TEXT("wss://") + Url.Mid(8);
	else if (Url.StartsWith(TEXT("http://"))) Url = TEXT("ws://") + Url.Mid(7);
	return FString::Printf(TEXT("%s/ws?token=%s&format=%s"), *Url, *FGenericPlatformHttp::UrlEncode(AccessToken),
		Encoding == EMusubiEncoding::Postcard ? TEXT("binary") : TEXT("json"));
}

void FMusubiSocket::Connect(const FString& BaseUrl, const FString& AccessToken, const FConfig& InConfig)
{
	Config = InConfig;
	Base = BaseUrl;
	Token = AccessToken;
	bUserClosed = false;
	Backoff = Config.BackoffMinSec;
	Open();
}

void FMusubiSocket::SetAccessToken(const FString& AccessToken)
{
	Token = AccessToken;
}

void FMusubiSocket::Open()
{
	if (Ws.IsValid())
	{
		Ws->OnConnected().RemoveAll(this);
		Ws->OnConnectionError().RemoveAll(this);
		Ws->OnClosed().RemoveAll(this);
		Ws->OnMessage().RemoveAll(this);
		Ws->OnRawMessage().RemoveAll(this);
		Ws.Reset();
	}
	BinaryAccum.Reset();
	FWebSocketsModule& Module = FModuleManager::LoadModuleChecked<FWebSocketsModule>(TEXT("WebSockets"));
	Ws = Module.CreateWebSocket(SocketUrl(Base, Token, Config.Encoding), TEXT(""));
	Ws->OnConnected().AddRaw(this, &FMusubiSocket::HandleConnected);
	Ws->OnConnectionError().AddRaw(this, &FMusubiSocket::HandleError);
	Ws->OnClosed().AddRaw(this, &FMusubiSocket::HandleClosed);
	Ws->OnMessage().AddRaw(this, &FMusubiSocket::HandleText);
	Ws->OnRawMessage().AddRaw(this, &FMusubiSocket::HandleBinary);
	SetState(bEverConnected ? EMusubiSocketState::Reconnecting : EMusubiSocketState::Connecting);
	Ws->Connect();
}

void FMusubiSocket::Close(int32 Code, const FString& Reason)
{
	bUserClosed = true;
	ReconnectAt = 0.0;
	if (Ws.IsValid() && Ws->IsConnected())
	{
		Ws->Close(Code, Reason);
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
	UE_LOG(LogMusubi, Warning, TEXT("socket error: %s"), *Error);
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

void FMusubiSocket::HandleBinary(const void* Data, SIZE_T Size, SIZE_T BytesRemaining)
{
	if (BinaryAccum.Num() + (int64)Size + (int64)BytesRemaining > Config.MaxFrameBytes)
	{
		Stats.DecodeErrors++;
		BinaryAccum.Reset();
		UE_LOG(LogMusubi, Warning, TEXT("binary frame over MaxFrameBytes, dropped"));
		return;
	}
	BinaryAccum.Append((const uint8*)Data, (int32)Size);
	if (BytesRemaining > 0) return;
	Stats.FramesIn++;
	Stats.BytesIn += BinaryAccum.Num();
	FMusubiServerEnvelope Env;
	const bool bOk = Env.FromPostcard(BinaryAccum.GetData(), BinaryAccum.Num());
	BinaryAccum.Reset();
	if (!bOk)
	{
		Stats.DecodeErrors++;
		UE_LOG(LogMusubi, Warning, TEXT("undecodable binary frame"));
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
	if (!IsConnected() || !Ws.IsValid()) return false;
	if (Config.Encoding == EMusubiEncoding::Postcard)
	{
		TArray<uint8> Bytes = Env.ToPostcard();
		if (Bytes.Num() == 0) return false;
		Stats.BytesOut += Bytes.Num();
		Ws->Send(Bytes.GetData(), Bytes.Num(), true);
	}
	else
	{
		const FString Text = Env.ToJsonText();
		Stats.BytesOut += Text.Len();
		Ws->Send(Text);
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
