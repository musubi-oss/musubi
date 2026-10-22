// Transport seam under FMusubiSocket: the socket owns the protocol (envelopes, cids, pings,
// reconnect); a transport only moves text/binary frames. WebSockets ship in this module; QUIC
// lives in the optional `MusubiQuic` module and registers itself here at startup, so the core
// has no link-time dependency on the native QUIC library.
#pragma once

#include "CoreMinimal.h"
#include "MusubiWire.generated.h"

enum class EMusubiTransport : uint8
{
	/** QUIC when the `MusubiQuic` module is loaded, WebSocket otherwise; falls back to WebSocket if QUIC fails to connect. */
	Auto,
	WebSocket,
	Quic,
};

struct MUSUBI_API FMusubiTransportArgs
{
	/** http(s) API base; the transport derives its own endpoint (`/ws`, or the node's QUIC port). */
	FString BaseUrl;
	FString AccessToken;
	EMusubiEncoding Encoding = EMusubiEncoding::Json;
	/** Ask the node for a resume token in `session` (realtime `hello.resumable`). */
	bool bResumable = false;
	/** Resume token from a previous `session`; the node binds the new link to the old session. */
	FString Resume;
	/** QUIC: UDP port when the node's QUIC listener is not on the API port. 0 = API port. */
	int32 QuicPort = 0;
	/** QUIC: accept any certificate (dev nodes with self-signed certs). */
	bool bInsecure = false;
	int32 MaxFrameBytes = 4 * 1024 * 1024;
};

DECLARE_MULTICAST_DELEGATE(FMusubiTransportConnected);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiTransportError, const FString&);
DECLARE_MULTICAST_DELEGATE_ThreeParams(FMusubiTransportClosed, int32 /*Code*/, const FString& /*Reason*/, bool /*bWasClean*/);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiTransportText, const FString&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiTransportBinary, const uint8*, int32);

class MUSUBI_API IMusubiTransport
{
public:
	virtual ~IMusubiTransport() = default;
	virtual void Connect(const FMusubiTransportArgs& Args) = 0;
	virtual void Close(int32 Code, const FString& Reason) = 0;
	virtual bool IsConnected() const = 0;
	virtual bool SendText(const FString& Text) = 0;
	/** `bUnreliable`: may travel on an unreliable channel (QUIC datagram) when the transport has one; else reliable. */
	virtual bool SendBinary(const uint8* Data, int32 Len, bool bUnreliable = false) = 0;
	/** Game-thread pump; delegates fire from here (or from the engine's own game-thread callbacks). */
	virtual void Tick(double DeltaSeconds) {}
	virtual EMusubiTransport Kind() const = 0;
	/** Whether SendBinary(…, true) actually uses an unreliable channel right now. */
	virtual bool HasUnreliable() const { return false; }

	FMusubiTransportConnected OnConnected;
	FMusubiTransportError OnError;
	FMusubiTransportClosed OnClosed;
	FMusubiTransportText OnText;
	FMusubiTransportBinary OnBinary;
};

class MUSUBI_API FMusubiTransportRegistry
{
public:
	typedef TFunction<TUniquePtr<IMusubiTransport>()> FFactory;
	/** Called by transport modules at startup (`MusubiQuic` registers EMusubiTransport::Quic). */
	static void Register(EMusubiTransport Kind, FFactory Factory);
	static void Unregister(EMusubiTransport Kind);
	static bool Has(EMusubiTransport Kind);
	/** Resolves Auto → Quic when registered, else WebSocket. Null only for an unregistered explicit kind. */
	static TUniquePtr<IMusubiTransport> Create(EMusubiTransport Kind);
	static EMusubiTransport Resolve(EMusubiTransport Kind);
};

/** Engine `IWebSocket` over `/ws?token=&format=`; token/format travel in the URL, there is no hello. */
class MUSUBI_API FMusubiWsTransport : public IMusubiTransport
{
public:
	~FMusubiWsTransport() override;
	void Connect(const FMusubiTransportArgs& Args) override;
	void Close(int32 Code, const FString& Reason) override;
	bool IsConnected() const override;
	bool SendText(const FString& Text) override;
	bool SendBinary(const uint8* Data, int32 Len, bool bUnreliable) override;
	EMusubiTransport Kind() const override { return EMusubiTransport::WebSocket; }

	static FString SocketUrl(const FMusubiTransportArgs& Args);

private:
	void Unbind();
	void HandleBinary(const void* Data, SIZE_T Size, SIZE_T BytesRemaining);

	TSharedPtr<class IWebSocket> Ws;
	TArray<uint8> Accum;
	int32 MaxFrameBytes = 4 * 1024 * 1024;
};
