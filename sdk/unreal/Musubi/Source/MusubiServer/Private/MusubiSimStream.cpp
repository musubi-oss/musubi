#include "MusubiSimStream.h"
#include "CborReader.h"
#include "CborWriter.h"
#include "Serialization/MemoryReader.h"
#include "Serialization/MemoryWriter.h"
#include "Misc/Base64.h"
#include "Math/Float16.h"

namespace MusubiCbor { void Write(class FCborWriter& W, const TSharedPtr<FJsonValue>& V, bool bAsBytes); }
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
		Ws->OnRawMessage().RemoveAll(this);
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
	Url = FString::Printf(TEXT("%s/v1/server/worlds/%s/stream?sim=%s%s%s"), *Base, *FGenericPlatformHttp::UrlEncode(World), *SimId.ToString(), Config.bDelta ? TEXT("&delta=1") : TEXT(""), Config.bCbor ? TEXT("&enc=cbor") : TEXT(""));
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
		Ws->OnRawMessage().RemoveAll(this);
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
	// the engine fires OnRawMessage for text frames too, so bind exactly one of the two: with `enc=cbor` the
	// node sends binary only, otherwise text only
	if (!Config.bCbor) Ws->OnMessage().AddLambda([Weak](const FString& Text) {
		if (TSharedPtr<FMusubiSimStream> S = Weak.Pin()) S->HandleText(Text);
	});
	// binary (cbor) frames arrive as fragments: reassemble until BytesRemaining == 0
	if (Config.bCbor) Ws->OnRawMessage().AddLambda([Weak](const void* Data, SIZE_T Size, SIZE_T BytesRemaining) {
		TSharedPtr<FMusubiSimStream> S = Weak.Pin();
		if (!S.IsValid()) return;
		S->RawBuf.Append(static_cast<const uint8*>(Data), (int32)Size);
		if (BytesRemaining == 0) { TArray<uint8> Whole = MoveTemp(S->RawBuf); S->RawBuf.Reset(); S->HandleBinary(Whole); }
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
	UE_LOG(LogMusubiServer, Log, TEXT("sim stream %s: %s"), *Url, S == EMusubiSimStreamState::Open ? TEXT("open") : S == EMusubiSimStreamState::Connecting ? TEXT("connecting") : S == EMusubiSimStreamState::Backoff ? TEXT("backoff") : TEXT("closed"));
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
	if (Config.bCbor)
	{
		TArray<uint8> Bytes;
		FMemoryWriter Ar(Bytes);
		FCborWriter W(&Ar, ECborEndianness::StandardCompliant);
		MusubiCbor::Write(W, FMusubiSimCommandFrame::ToJson(F), false);
		UE_LOG(LogMusubiServer, Verbose, TEXT("sim stream -> cbor %d B %s"), Bytes.Num(), *F.ToJsonText().Left(200));
		Ws->Send(Bytes.GetData(), Bytes.Num(), true);
	}
	else
	{
		const FString Text = F.ToJsonText();
		UE_LOG(LogMusubiServer, Verbose, TEXT("sim stream -> %s"), *Text.Left(300));
		Ws->Send(Text);
	}
	Stats.CommandsOut++;
	return true;
}

namespace MusubiCbor
{
// Component names whose base64 strings go over the wire as CBOR byte strings (the node's `bin` default).
static bool IsBinaryComponent(const FString& K) { return K == TEXT("iris") || K == TEXT("iris.d"); }

// JSON -> CBOR. `bAsBytes`: this string is a base64 blob, write it as a byte string (text if it is not base64).
void Write(FCborWriter& W, const TSharedPtr<FJsonValue>& V, bool bAsBytes)
{
	if (!V.IsValid() || V->Type == EJson::Null || V->Type == EJson::None) { W.WriteNull(); return; }
	switch (V->Type)
	{
	case EJson::Boolean: W.WriteValue(V->AsBool()); return;
	case EJson::Number:
	{
		const double D = V->AsNumber();
		if (D == FMath::FloorToDouble(D) && FMath::Abs(D) < 9.0e15) W.WriteValue((int64)D); else W.WriteValue(D);
		return;
	}
	case EJson::String:
	{
		const FString S = V->AsString();
		TArray<uint8> B;
		if (bAsBytes && FBase64::Decode(S, B)) { W.WriteValue(B.GetData(), (uint64)B.Num()); return; }
		W.WriteValue(S);
		return;
	}
	case EJson::Array:
	{
		const TArray<TSharedPtr<FJsonValue>>& A = V->AsArray();
		// the engine writer never pops a zero-length container (it pops on the item that brings the count to 0),
		// so an empty container goes out indefinite + break
		if (A.Num() == 0) { W.WriteContainerStart(ECborCode::Array, -1); W.WriteContainerEnd(); return; }
		W.WriteContainerStart(ECborCode::Array, A.Num());
		for (const TSharedPtr<FJsonValue>& X : A) Write(W, X, false);
		return;
	}
	case EJson::Object:
	{
		const TSharedPtr<FJsonObject>& O = V->AsObject();
		if (O->Values.Num() == 0) { W.WriteContainerStart(ECborCode::Map, -1); W.WriteContainerEnd(); return; }
		W.WriteContainerStart(ECborCode::Map, O->Values.Num());
		FString Name;
		const bool bNamed = O->HasField(TEXT("value")) && O->TryGetStringField(TEXT("name"), Name) && IsBinaryComponent(Name);
		for (const auto& KV : O->Values)
		{
			const FString Key(KV.Key); // 5.8 keys FJsonObject by UE::TSharedString, older engines by FString
			W.WriteValue(Key);
			Write(W, KV.Value, (bNamed && Key == TEXT("value")) || IsBinaryComponent(Key));
		}
		return;
	}
	default: W.WriteNull(); return;
	}
}

static TSharedPtr<FJsonValue> ReadValue(FCborReader& R, const FCborContext& C, FArchive& Ar, const TArray<uint8>& Bytes);

static TSharedPtr<FJsonValue> ReadNext(FCborReader& R, FArchive& Ar, const TArray<uint8>& Bytes)
{
	FCborContext C;
	if (!R.ReadNext(C)) return nullptr;
	return ReadValue(R, C, Ar, Bytes);
}

// CBOR -> JSON; byte strings become base64 strings, which is what the JSON frame carried before.
static TSharedPtr<FJsonValue> ReadValue(FCborReader& R, const FCborContext& C, FArchive& Ar, const TArray<uint8>& Bytes)
{
	switch (C.MajorType())
	{
	case ECborCode::Uint: return MakeShared<FJsonValueNumber>((double)C.AsUInt());
	case ECborCode::Int: return MakeShared<FJsonValueNumber>((double)C.AsInt());
	case ECborCode::TextString: return MakeShared<FJsonValueString>(C.AsString());
	case ECborCode::ByteString: { TArrayView<const uint8> B = C.AsByteArray(); return MakeShared<FJsonValueString>(FBase64::Encode(B.GetData(), B.Num())); }
	case ECborCode::Array:
	{
		TArray<TSharedPtr<FJsonValue>> Out;
		// the engine reader reports the end of a finite container as a synthetic Break too, so one loop serves both
		for (;;) { FCborContext E; if (!R.ReadNext(E) || E.IsBreak()) break; Out.Add(ReadValue(R, E, Ar, Bytes)); }
		return MakeShared<FJsonValueArray>(Out);
	}
	case ECborCode::Map:
	{
		TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
		for (;;)
		{
			FCborContext K;
			if (!R.ReadNext(K) || K.IsBreak()) break;
			const FString Key = K.MajorType() == ECborCode::TextString ? K.AsString()
				: (K.MajorType() == ECborCode::Uint || K.MajorType() == ECborCode::Int) ? FString::Printf(TEXT("%lld"), (long long)K.AsInt())
				: FString();
			TSharedPtr<FJsonValue> X = ReadNext(R, Ar, Bytes);
			if (!X.IsValid()) break;
			O->SetField(Key, X);
		}
		return MakeShared<FJsonValueObject>(O);
	}
	case ECborCode::Tag: return ReadNext(R, Ar, Bytes);
	case ECborCode::Prim:
		switch (C.AdditionalValue())
		{
		case ECborCode::False: return MakeShared<FJsonValueBoolean>(false);
		case ECborCode::True: return MakeShared<FJsonValueBoolean>(true);
		case ECborCode::Value_2Bytes:
		{
			// half float: ciborium shrinks every float that survives the round trip (300.0, 0.5, ...) to 16 bits; the
			// engine's context has no accessor for it, so take the two payload bytes the reader just consumed
			const int64 Pos = Ar.Tell();
			FFloat16 H;
			H.Encoded = Pos >= 2 && Pos <= Bytes.Num() ? (uint16)((Bytes[Pos - 2] << 8) | Bytes[Pos - 1]) : 0;
			return MakeShared<FJsonValueNumber>((double)H.GetFloat());
		}
		case ECborCode::Value_4Bytes: return MakeShared<FJsonValueNumber>((double)C.AsFloat());
		case ECborCode::Value_8Bytes: return MakeShared<FJsonValueNumber>(C.AsDouble());
		default: return MakeShared<FJsonValueNull>();
		}
	default: return MakeShared<FJsonValueNull>();
	}
}
}

void FMusubiSimStream::HandleBinary(const TArray<uint8>& Bytes)
{
	FMemoryReader Ar(Bytes);
	FCborReader R(&Ar, ECborEndianness::StandardCompliant);
	TSharedPtr<FJsonValue> J = MusubiCbor::ReadNext(R, Ar, Bytes);
	if (!J.IsValid())
	{
		FString Hex;
		for (int32 i = 0; i < FMath::Min(Bytes.Num(), 16); ++i) Hex += FString::Printf(TEXT("%02x"), Bytes[i]);
		UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: bad cbor frame (%d B, error 0x%02x, head %s)"), Bytes.Num(), (int32)R.GetError().RawCode(), *Hex);
		Stats.Errors++;
		return;
	}
	Stats.CborFramesIn++;
	Stats.CborBytesIn += Bytes.Num();
	if (Stats.CborFramesIn % 300 == 0)
		UE_LOG(LogMusubiServer, Log, TEXT("sim stream: cbor frames=%llu bytes=%llu avg=%llu B"), Stats.CborFramesIn, Stats.CborBytesIn, Stats.CborBytesIn / Stats.CborFramesIn);
	UE_LOG(LogMusubiServer, VeryVerbose, TEXT("sim stream <- cbor %d B"), Bytes.Num());
	HandleFrame(J, Bytes.Num());
}

void FMusubiSimStream::HandleText(const FString& Text)
{
	UE_LOG(LogMusubiServer, VeryVerbose, TEXT("sim stream <- %s"), *Text.Left(200));
	HandleFrame(MusubiJson::Deserialize(Text), Text.Len());
}

void FMusubiSimStream::HandleFrame(const TSharedPtr<FJsonValue>& J, int32 WireBytes)
{
	const TSharedPtr<FJsonObject>* OP;
	if (!J.IsValid() || !J->TryGetObject(OP)) { UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: non-object frame")); return; }
	const TSharedPtr<FJsonObject>& O = *OP;
	FString Type;
	if (O->TryGetStringField(TEXT("type"), Type))
	{
		if (Type == TEXT("tick"))
		{
			FMusubiSimFrame F;
			if (!FMusubiSimFrame::FromJson(J, F) || !F.Tick.IsSet())
			{
				FString Dump;
				{ auto W = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Dump); FJsonSerializer::Serialize(J, TEXT(""), W, true); }
				UE_LOG(LogMusubiServer, Warning, TEXT("sim stream: bad tick frame: %s"), *Dump.Left(900));
				Stats.Errors++;
				return;
			}
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
