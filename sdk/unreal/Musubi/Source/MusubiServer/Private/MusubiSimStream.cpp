#include "MusubiSimStream.h"
#include "MusubiServerModule.h"
#include "MusubiJson.h"
#include "IWebSocket.h"
#include "WebSocketsModule.h"
#include "Modules/ModuleManager.h"
#include "GenericPlatform/GenericPlatformHttp.h"

FMusubiSimStream::~FMusubiSimStream()
{
	if (Ws.IsValid())
	{
		Ws->OnConnected().RemoveAll(this);
		Ws->OnConnectionError().RemoveAll(this);
		Ws->OnClosed().RemoveAll(this);
		Ws->OnMessage().RemoveAll(this);
		if (Ws->IsConnected()) Ws->Close();
	}
}

void FMusubiSimStream::Connect(const FString& ApiBase, const FString& ServerKey, const FString& World, const FMusubiUuid& SimId, const FConfig& InConfig)
{
	Config = InConfig;
	Key = ServerKey;
	WorldName = World;
	Sim = SimId;
	FString Base = ApiBase;
	Base.RemoveFromEnd(TEXT("/"));
	if (Base.StartsWith(TEXT("https://"))) Base = TEXT("wss://") + Base.Mid(8);
	else if (Base.StartsWith(TEXT("http://"))) Base = TEXT("ws://") + Base.Mid(7);
	Url = FString::Printf(TEXT("%s/v1/server/worlds/%s/stream?sim=%s%s"), *Base, *FGenericPlatformHttp::UrlEncode(World), *SimId.ToString(), Config.bDelta ? TEXT("&delta=1") : TEXT(""));
	Backoff = Config.BackoffMinSec;
	Open();
}

void FMusubiSimStream::Open()
{
	if (Ws.IsValid())
	{
		Ws->OnConnected().RemoveAll(this);
		Ws->OnConnectionError().RemoveAll(this);
		Ws->OnClosed().RemoveAll(this);
		Ws->OnMessage().RemoveAll(this);
		Ws.Reset();
	}
	FWebSocketsModule& Module = FModuleManager::LoadModuleChecked<FWebSocketsModule>(TEXT("WebSockets"));
	TMap<FString, FString> Headers;
	Headers.Add(TEXT("x-musubi-server-key"), Key);
	Ws = Module.CreateWebSocket(Url, TArray<FString>(), Headers);
	TWeakPtr<FMusubiSimStream> Weak = AsShared();
	Ws->OnConnected().AddLambda([Weak]() {
		if (TSharedPtr<FMusubiSimStream> S = Weak.Pin()) { S->Backoff = S->Config.BackoffMinSec; S->SetState(EMusubiSimStreamState::Open); }
	});
	Ws->OnConnectionError().AddLambda([Weak](const FString& Err) {
		if (TSharedPtr<FMusubiSimStream> S = Weak.Pin())
		{
			UE_LOG(LogMusubiServer, Warning, TEXT("sim stream connect failed: %s"), *Err);
			S->Stats.Errors++;
			S->FailPending(TEXT("connect failed"));
			S->ScheduleReconnect();
		}
	});
	Ws->OnClosed().AddLambda([Weak](int32 Code, const FString& Reason, bool) {
		if (TSharedPtr<FMusubiSimStream> S = Weak.Pin())
		{
			UE_LOG(LogMusubiServer, Log, TEXT("sim stream closed %d %s"), Code, *Reason);
			S->FailPending(TEXT("closed"));
			if (S->State != EMusubiSimStreamState::Closed) S->ScheduleReconnect();
		}
	});
	Ws->OnMessage().AddLambda([Weak](const FString& Text) {
		if (TSharedPtr<FMusubiSimStream> S = Weak.Pin()) S->HandleText(Text);
	});
	SetState(EMusubiSimStreamState::Connecting);
	Ws->Connect();
}

void FMusubiSimStream::Close()
{
	SetState(EMusubiSimStreamState::Closed);
	FailPending(TEXT("closed by simulator"));
	if (Ws.IsValid() && Ws->IsConnected()) Ws->Close(1000, TEXT("simulator stopped"));
}

void FMusubiSimStream::SetState(EMusubiSimStreamState S)
{
	if (State == S) return;
	State = S;
	OnStateChanged.Broadcast(S);
}

void FMusubiSimStream::ScheduleReconnect()
{
	if (!Config.bAutoReconnect) { SetState(EMusubiSimStreamState::Closed); return; }
	Stats.Reconnects++;
	BackoffUntil = Now + Backoff;
	Backoff = FMath::Min(Config.BackoffMaxSec, Backoff * 2.0);
	SetState(EMusubiSimStreamState::Backoff);
}

void FMusubiSimStream::Tick(double DeltaSeconds)
{
	Now += DeltaSeconds;
	if (State == EMusubiSimStreamState::Backoff && Now >= BackoffUntil) Open();
	if (Pending.Num())
	{
		TArray<uint64> Late;
		for (const auto& KV : Pending) if (Now - KV.Value.SentAt > Config.ReplyTimeoutSec) Late.Add(KV.Key);
		for (uint64 Id : Late)
		{
			FPending P;
			Pending.RemoveAndCopyValue(Id, P);
			TSharedPtr<FJsonObject> R = MakeShared<FJsonObject>();
			R->SetStringField(TEXT("op"), P.Op);
			R->SetStringField(TEXT("error"), TEXT("reply timeout"));
			if (P.OnReply) P.OnReply(false, R);
		}
	}
}

void FMusubiSimStream::FailPending(const FString& Why)
{
	TMap<uint64, FPending> Old = MoveTemp(Pending);
	Pending.Reset();
	for (auto& KV : Old)
	{
		TSharedPtr<FJsonObject> R = MakeShared<FJsonObject>();
		R->SetStringField(TEXT("op"), KV.Value.Op);
		R->SetStringField(TEXT("error"), Why);
		if (KV.Value.OnReply) KV.Value.OnReply(false, R);
	}
}

bool FMusubiSimStream::Send(const FMusubiSimCommand& Cmd, FMusubiSimReply OnReply)
{
	if (!IsOpen() || !Ws.IsValid())
	{
		if (OnReply)
		{
			TSharedPtr<FJsonObject> R = MakeShared<FJsonObject>();
			R->SetStringField(TEXT("error"), TEXT("stream not open"));
			OnReply(false, R);
		}
		return false;
	}
	FMusubiSimCommandFrame F;
	F.Cmd = Cmd;
	const uint64 Id = NextId++;
	FMusubiJson IdJ;
	IdJ.Value = MakeShared<FJsonValueNumber>((double)Id);
	F.Id = IdJ;
	Pending.Add(Id, FPending{MoveTemp(OnReply), Now, FString(MusubiTagName(Cmd.Tag))});
	Ws->Send(F.ToJsonText());
	Stats.CommandsOut++;
	return true;
}

void FMusubiSimStream::HandleText(const FString& Text)
{
	TSharedPtr<FJsonValue> J = MusubiJson::Deserialize(Text);
	const TSharedPtr<FJsonObject>* OP;
	if (!J.IsValid() || !J->TryGetObject(OP)) { UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: non-object frame")); return; }
	const TSharedPtr<FJsonObject>& O = *OP;
	FString Type;
	if (O->TryGetStringField(TEXT("type"), Type))
	{
		if (Type == TEXT("tick"))
		{
			FMusubiSimFrame F;
			if (!FMusubiSimFrame::FromJson(J, F) || !F.Tick.IsSet()) { UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: bad tick frame")); Stats.Errors++; return; }
			Stats.FramesIn++;
			Stats.LastFrameAt = Now;
			OnFrame.Broadcast(F.Tick.GetValue());
		}
		else if (Type == TEXT("error"))
		{
			double T = 0; FString Err;
			O->TryGetNumberField(TEXT("tick"), T);
			O->TryGetStringField(TEXT("error"), Err);
			Stats.Errors++;
			OnFrameError.Broadcast((uint64)T, Err);
		}
		return;
	}
	// a reply: {op, id?, ...}
	Stats.RepliesIn++;
	double IdNum = 0;
	if (!O->TryGetNumberField(TEXT("id"), IdNum))
	{
		FString Err;
		if (O->TryGetStringField(TEXT("error"), Err)) UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: %s"), *Err);
		return;
	}
	FPending P;
	if (!Pending.RemoveAndCopyValue((uint64)IdNum, P)) return;
	FString Err, Redirect;
	const bool bErr = O->TryGetStringField(TEXT("error"), Err);
	if (O->TryGetStringField(TEXT("redirect"), Redirect))
	{
		FString Cells;
		RedirectCache.Add(P.Op, Redirect);
	}
	if (P.OnReply) P.OnReply(!bErr && Redirect.IsEmpty(), O);
}
