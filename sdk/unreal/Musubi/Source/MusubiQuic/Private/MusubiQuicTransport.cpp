#include "MusubiQuicTransport.h"
#include "MusubiQuicModule.h"

#if MUSUBI_WITH_QUIC
#include "musubi_quic.h"
#include "Dom/JsonObject.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Policies/CondensedJsonPrintPolicy.h"

FMusubiQuicTransport::~FMusubiQuicTransport()
{
	Drop();
}

void FMusubiQuicTransport::Drop()
{
	if (Conn)
	{
		mq_close(Conn);
		Conn = nullptr;
	}
	bConnected = false;
	bClosing = false;
}

bool FMusubiQuicTransport::Endpoint(const FMusubiTransportArgs& Args, FString& OutHost, int32& OutPort, bool& OutTls)
{
	FString Url = Args.BaseUrl;
	Url.RemoveFromEnd(TEXT("/"));
	OutTls = true;
	if (Url.StartsWith(TEXT("https://"))) Url = Url.Mid(8);
	else if (Url.StartsWith(TEXT("http://"))) { Url = Url.Mid(7); OutTls = false; }
	int32 Slash;
	if (Url.FindChar(TEXT('/'), Slash)) Url = Url.Left(Slash);
	OutPort = OutTls ? 443 : 80;
	// host[:port]; bracketed IPv6 allowed
	int32 Colon;
	if (Url.StartsWith(TEXT("[")))
	{
		int32 Close;
		if (!Url.FindChar(TEXT(']'), Close)) return false;
		OutHost = Url.Mid(1, Close - 1);
		if (Url.Len() > Close + 1 && Url[Close + 1] == TEXT(':')) OutPort = FCString::Atoi(*Url.Mid(Close + 2));
	}
	else if (Url.FindLastChar(TEXT(':'), Colon))
	{
		OutHost = Url.Left(Colon);
		OutPort = FCString::Atoi(*Url.Mid(Colon + 1));
	}
	else
	{
		OutHost = Url;
	}
	if (Args.QuicPort > 0) OutPort = Args.QuicPort;
	return !OutHost.IsEmpty() && OutPort > 0 && OutPort < 65536;
}

FString FMusubiQuicTransport::HelloJson(const FMusubiTransportArgs& Args)
{
	TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
	O->SetStringField(TEXT("token"), Args.AccessToken);
	O->SetStringField(TEXT("format"), Args.Encoding == EMusubiEncoding::Postcard ? TEXT("binary") : TEXT("json"));
	if (Args.bResumable) O->SetBoolField(TEXT("resumable"), true);
	if (!Args.Resume.IsEmpty()) O->SetStringField(TEXT("resume"), Args.Resume);
	FString Out;
	TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> W = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Out);
	FJsonSerializer::Serialize(O.ToSharedRef(), W);
	return Out;
}

void FMusubiQuicTransport::Connect(const FMusubiTransportArgs& Args)
{
	Drop();
	MaxFrameBytes = Args.MaxFrameBytes;
	FString Host;
	int32 Port;
	bool bTls;
	if (!Endpoint(Args, Host, Port, bTls))
	{
		OnError.Broadcast(FString::Printf(TEXT("quic: cannot derive endpoint from %s"), *Args.BaseUrl));
		return;
	}
	// QUIC is always TLS; a plain http:// base means a dev node, so accept its self-signed certificate.
	const bool bInsecure = Args.bInsecure || !bTls;
	if (bInsecure)
		UE_LOG(LogMusubiQuic, Warning, TEXT("quic %s:%d: certificate verification disabled"), *Host, Port);
	PendingHello = HelloJson(Args);
	Conn = mq_connect(TCHAR_TO_UTF8(*Host), (uint16)Port, TCHAR_TO_UTF8(*Host), bInsecure ? 1 : 0, nullptr);
	if (!Conn)
	{
		OnError.Broadcast(FString::Printf(TEXT("quic: cannot resolve %s:%d"), *Host, Port));
		return;
	}
	UE_LOG(LogMusubiQuic, Log, TEXT("quic connecting %s:%d"), *Host, Port);
}

void FMusubiQuicTransport::Close(int32 Code, const FString& Reason)
{
	if (!Conn)
	{
		OnClosed.Broadcast(Code, Reason, true);
		return;
	}
	// The ffi sends the protocol close frame and the CONNECTION_CLOSE; report synchronously like
	// an already-closed WebSocket so the socket's state machine does not wait on a dead handle.
	Drop();
	OnClosed.Broadcast(Code, Reason, true);
}

bool FMusubiQuicTransport::SendText(const FString& Text)
{
	if (!Conn || !bConnected) return false;
	FTCHARToUTF8 Utf8(*Text);
	return mq_send(Conn, 0, (const uint8*)Utf8.Get(), (uint32)Utf8.Length(), 0) == 0;
}

bool FMusubiQuicTransport::SendBinary(const uint8* Data, int32 Len, bool bUnreliable)
{
	if (!Conn || !bConnected || Len < 0) return false;
	return mq_send(Conn, 1, Data, (uint32)Len, bUnreliable ? 1 : 0) == 0;
}

bool FMusubiQuicTransport::HasUnreliable() const
{
	return Conn && bConnected && mq_max_datagram(Conn) > 0;
}

void FMusubiQuicTransport::Tick(double /*DeltaSeconds*/)
{
	if (!Conn) return;
	MqEvent Ev;
	for (int32 N = 0; N < MaxEventsPerTick && Conn && mq_poll(Conn, &Ev) > 0; ++N)
	{
		switch (Ev.kind)
		{
		case MQ_EVENT_CONNECTED:
		{
			// hello first, then the socket may send envelopes
			FTCHARToUTF8 Utf8(*PendingHello);
			mq_send(Conn, 0, (const uint8*)Utf8.Get(), (uint32)Utf8.Length(), 0);
			PendingHello.Reset();
			bConnected = true;
			OnConnected.Broadcast();
			break;
		}
		case MQ_EVENT_FRAME:
			if ((int32)Ev.len > MaxFrameBytes)
			{
				OnError.Broadcast(TEXT("quic frame over MaxFrameBytes"));
				break;
			}
			if (Ev.frame_type == 0)
			{
				FUTF8ToTCHAR Conv((const ANSICHAR*)Ev.data, (int32)Ev.len);
				OnText.Broadcast(FString(Conv.Length(), Conv.Get()));
			}
			else
			{
				OnBinary.Broadcast(Ev.data, (int32)Ev.len);
			}
			break;
		case MQ_EVENT_CLOSED:
		{
			const FString Reason = FString(FUTF8ToTCHAR((const ANSICHAR*)Ev.data, (int32)Ev.len).Get());
			const bool bWas = bConnected;
			Drop();
			if (bWas) OnClosed.Broadcast(1000, Reason, true);
			else OnError.Broadcast(FString::Printf(TEXT("quic closed before handshake: %s"), *Reason));
			return;
		}
		case MQ_EVENT_ERROR:
		{
			const FString Msg = FString(FUTF8ToTCHAR((const ANSICHAR*)Ev.data, (int32)Ev.len).Get());
			const bool bWas = bConnected;
			Drop();
			if (bWas) OnClosed.Broadcast(1006, Msg, false);
			else OnError.Broadcast(Msg);
			return;
		}
		default:
			break;
		}
	}
}
#endif
