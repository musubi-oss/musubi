#pragma once

#include "CoreMinimal.h"
#include "Engine/NetConnection.h"
#include "MusubiJson.h"
#include "MusubiNetConnection.generated.h"

/**
 * One engine connection relayed by the node (`net_*` messages). The driver hands packets in through
 * `ReceivedRawPacket` and takes them out through `LowLevelSend`; this class only knows its relay id
 * and, on the server, which client session answers to it.
 */
UCLASS(transient, config = Engine)
class MUSUBINET_API UMusubiNetConnection : public UNetConnection
{
	GENERATED_BODY()

public:
	UMusubiNetConnection(const FObjectInitializer& ObjectInitializer);

	virtual void InitBase(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void InitLocalConnection(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void InitRemoteConnection(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, const class FInternetAddr& InRemoteAddr, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void LowLevelSend(void* Data, int32 CountBits, FOutPacketTraits& Traits) override;
	virtual FString LowLevelGetRemoteAddress(bool bAppendPort = false) override;
	virtual FString LowLevelDescribe() override;
	virtual void CleanUp() override;

	/** Relay id from `net_opened` (client) or the first `net_packet` event (server). 0 = not open yet. */
	uint64 ConnId = 0;
	/** Client side: the world/entity the relay was opened on. */
	FString World, Entity;
	/** Server side: the client session the node routes our packets back to. */
	FMusubiJson Session;
	/** Client side: packets written before `net_opened`, flushed on open. */
	TArray<TArray<uint8>> PendingOut;
	/** Set by the driver before CleanUp() on a rehome: the relay conn lives on for the next engine connection. */
	bool bKeepRelay = false;

	bool IsOpen() const { return ConnId != 0; }
	/** Client side: the relay is open — set the id/limit and flush what was queued. */
	void Opened(uint64 InConnId, int32 InMaxPacket);
};
