// IMusubiTransport over musubi-quic-ffi. The hello is the first text frame on the bidirectional
// stream (node: realtime/quic.rs `handle`): `{"token","format","resumable","resume"}`; afterwards
// the stream carries envelopes exactly like the WebSocket, and binary frames flagged unreliable go
// out as QUIC datagrams when they fit (`mq_max_datagram`), otherwise on the stream like the node does.
#pragma once

#include "CoreMinimal.h"
#include "MusubiTransport.h"

#if MUSUBI_WITH_QUIC
struct MqConn;

class FMusubiQuicTransport : public IMusubiTransport
{
public:
	~FMusubiQuicTransport() override;
	void Connect(const FMusubiTransportArgs& Args) override;
	void Close(int32 Code, const FString& Reason) override;
	bool IsConnected() const override { return bConnected; }
	bool SendText(const FString& Text) override;
	bool SendBinary(const uint8* Data, int32 Len, bool bUnreliable) override;
	void Tick(double DeltaSeconds) override;
	EMusubiTransport Kind() const override { return EMusubiTransport::Quic; }
	bool HasUnreliable() const override;

	/** `host`, `port`, `sni` for the node behind an http(s) API base; QuicPort overrides the URL port. */
	static bool Endpoint(const FMusubiTransportArgs& Args, FString& OutHost, int32& OutPort, bool& OutTls);
	static FString HelloJson(const FMusubiTransportArgs& Args);

private:
	void Drop();

	MqConn* Conn = nullptr;
	bool bConnected = false;
	/** When the handshake started; a node without QUIC answers nothing, so give up well before the 60 s idle timeout. */
	double ConnectStarted = 0;
	double HandshakeTimeoutSec = 5.0;
	bool bClosing = false;
	FString PendingHello;
	int32 MaxFrameBytes = 4 * 1024 * 1024;
	/** Per-tick cap on frames handed to the socket so a burst cannot stall the game thread. */
	static constexpr int32 MaxEventsPerTick = 512;
};
#endif
