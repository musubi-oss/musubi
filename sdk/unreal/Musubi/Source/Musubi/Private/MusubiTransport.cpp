#include "MusubiTransport.h"
#include "IWebSocket.h"
#include "WebSocketsModule.h"
#include "GenericPlatform/GenericPlatformHttp.h"
#include "Modules/ModuleManager.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiTransport, Log, All);

namespace
{
	TMap<EMusubiTransport, FMusubiTransportRegistry::FFactory>& Factories()
	{
		static TMap<EMusubiTransport, FMusubiTransportRegistry::FFactory> Map;
		return Map;
	}
}

void FMusubiTransportRegistry::Register(EMusubiTransport Kind, FFactory Factory)
{
	Factories().Add(Kind, MoveTemp(Factory));
}

void FMusubiTransportRegistry::Unregister(EMusubiTransport Kind)
{
	Factories().Remove(Kind);
}

bool FMusubiTransportRegistry::Has(EMusubiTransport Kind)
{
	return Kind == EMusubiTransport::WebSocket || Factories().Contains(Kind);
}

EMusubiTransport FMusubiTransportRegistry::Resolve(EMusubiTransport Kind)
{
	if (Kind == EMusubiTransport::Auto)
		return Has(EMusubiTransport::Quic) ? EMusubiTransport::Quic : EMusubiTransport::WebSocket;
	return Kind;
}

TUniquePtr<IMusubiTransport> FMusubiTransportRegistry::Create(EMusubiTransport Kind)
{
	Kind = Resolve(Kind);
	if (Kind == EMusubiTransport::WebSocket) return MakeUnique<FMusubiWsTransport>();
	if (const FFactory* F = Factories().Find(Kind)) return (*F)();
	return nullptr;
}

// ---- WebSocket --------------------------------------------------------------------------------

FMusubiWsTransport::~FMusubiWsTransport()
{
	Unbind();
	if (Ws.IsValid() && Ws->IsConnected()) Ws->Close();
}

FString FMusubiWsTransport::SocketUrl(const FMusubiTransportArgs& Args)
{
	FString Url = Args.BaseUrl;
	Url.RemoveFromEnd(TEXT("/"));
	if (Url.StartsWith(TEXT("https://"))) Url = TEXT("wss://") + Url.Mid(8);
	else if (Url.StartsWith(TEXT("http://"))) Url = TEXT("ws://") + Url.Mid(7);
	Url = FString::Printf(TEXT("%s/ws?token=%s&format=%s"), *Url, *FGenericPlatformHttp::UrlEncode(Args.AccessToken),
		Args.Encoding == EMusubiEncoding::Postcard ? TEXT("binary") : TEXT("json"));
	if (Args.bResumable) Url += TEXT("&resumable=1");
	if (!Args.Resume.IsEmpty()) Url += TEXT("&resume=") + FGenericPlatformHttp::UrlEncode(Args.Resume);
	return Url;
}

void FMusubiWsTransport::Unbind()
{
	if (!Ws.IsValid()) return;
	Ws->OnConnected().RemoveAll(this);
	Ws->OnConnectionError().RemoveAll(this);
	Ws->OnClosed().RemoveAll(this);
	Ws->OnMessage().RemoveAll(this);
	Ws->OnRawMessage().RemoveAll(this);
}

void FMusubiWsTransport::Connect(const FMusubiTransportArgs& Args)
{
	Unbind();
	Ws.Reset();
	Accum.Reset();
	MaxFrameBytes = Args.MaxFrameBytes;
	FWebSocketsModule& Module = FModuleManager::LoadModuleChecked<FWebSocketsModule>(TEXT("WebSockets"));
	Ws = Module.CreateWebSocket(SocketUrl(Args), TEXT(""));
	Ws->OnConnected().AddLambda([this]() { OnConnected.Broadcast(); });
	Ws->OnConnectionError().AddLambda([this](const FString& E) { OnError.Broadcast(E); });
	Ws->OnClosed().AddLambda([this](int32 Code, const FString& Reason, bool bClean) { OnClosed.Broadcast(Code, Reason, bClean); });
	Ws->OnMessage().AddLambda([this](const FString& T) { OnText.Broadcast(T); });
	Ws->OnRawMessage().AddRaw(this, &FMusubiWsTransport::HandleBinary);
	Ws->Connect();
}

void FMusubiWsTransport::HandleBinary(const void* Data, SIZE_T Size, SIZE_T BytesRemaining)
{
	if (Accum.Num() + (int64)Size + (int64)BytesRemaining > MaxFrameBytes)
	{
		Accum.Reset();
		OnError.Broadcast(TEXT("binary frame over MaxFrameBytes"));
		return;
	}
	Accum.Append((const uint8*)Data, (int32)Size);
	if (BytesRemaining > 0) return;
	OnBinary.Broadcast(Accum.GetData(), Accum.Num());
	Accum.Reset();
}

void FMusubiWsTransport::Close(int32 Code, const FString& Reason)
{
	if (Ws.IsValid() && Ws->IsConnected()) Ws->Close(Code, Reason);
	else OnClosed.Broadcast(Code, Reason, true);
}

bool FMusubiWsTransport::IsConnected() const
{
	return Ws.IsValid() && Ws->IsConnected();
}

bool FMusubiWsTransport::SendText(const FString& Text)
{
	if (!IsConnected()) return false;
	Ws->Send(Text);
	return true;
}

bool FMusubiWsTransport::SendBinary(const uint8* Data, int32 Len, bool /*bUnreliable*/)
{
	if (!IsConnected()) return false;
	Ws->Send(Data, Len, true);
	return true;
}
