#pragma once

#include "CoreMinimal.h"
#include "Engine/NetDriver.h"
#include "MusubiEnvelope.h"
#include "MusubiSimWire.generated.h"
#include "MusubiNetDriver.generated.h"

class UMusubiNetConnection;
class UMusubiSubsystem;
class UMusubiSimulatorSubsystem;

/**
 * Engine networking through the Musubi node (docs/gdk-unreal.md §9.2).
 *
 * Client: `musubi://<world>?entity=<uuid>` — `net_open` on the entity, then every engine packet is a
 * `net_packet` the node routes to whichever simulator owns the entity right now; after a handover the
 * node sends `net_rehome` and the driver restarts the engine connection (`OnRehome`).
 * Server (dedicated, with UMusubiSimulatorSubsystem running): `InitListen` opens nothing; `net_packet`
 * events on the sim stream open connections implicitly (first packet on an unknown conn, as UIpNetDriver
 * does for a new address) and `net_close` closes them. No stateless-connect challenge: the node already
 * authenticated the session. Replication stays the engine's (Iris or classic) — this class is transport.
 *
 *   [/Script/Engine.Engine]
 *   +NetDriverDefinitions=(DefName="GameNetDriver",DriverClassName="/Script/MusubiNet.MusubiNetDriver",DriverClassNameFallback="/Script/OnlineSubsystemUtils.IpNetDriver",bCanUseIris=true)
 */
UCLASS(transient, config = Engine)
class MUSUBINET_API UMusubiNetDriver : public UNetDriver
{
	GENERATED_BODY()

public:
	UMusubiNetDriver(const FObjectInitializer& ObjectInitializer);

	virtual bool IsAvailable() const override { return true; }
	virtual bool InitBase(bool bInitAsClient, FNetworkNotify* InNotify, const FURL& URL, bool bReuseAddressAndPort, FString& Error) override;
	virtual bool InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error) override;
	virtual bool InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error) override;
	virtual void TickDispatch(float DeltaTime) override;
	virtual void LowLevelSend(TSharedPtr<const FInternetAddr> Address, void* Data, int32 CountBits, FOutPacketTraits& Traits) override;
	virtual FString LowLevelGetNetworkNumber() override;
	virtual void LowLevelDestroy() override;
	virtual bool IsNetResourceValid() override;
	virtual class ISocketSubsystem* GetSocketSubsystem() override;

	/** Transport for one connection's bytes: `net_packet` up (client) or a `net_packet` command (server). */
	void SendPacket(UMusubiNetConnection* Conn, const uint8* Data, int32 Count);

	/** Largest `data` the node relays; also the connection's MaxPacket. */
	UPROPERTY(Config)
	int32 MaxRelayPacket = 1200;
	/** Client: `net_rehome` → how long the restarted connection may take to get `NMT_Welcome` before it is a failure. */
	UPROPERTY(Config)
	float RehomeTimeout = 5.f;

protected:
	/** Client: the entity moved to another simulator. Default: travel to the same URL (`TRAVEL_Absolute`), i.e. a reconnect; projects override for an in-place restart. */
	virtual void OnRehome(const FMusubiServerMessageNetRehome& Msg);

private:
	// client
	TWeakObjectPtr<UMusubiSubsystem> Client;
	FDelegateHandle PacketHandle, CloseHandle, RehomeHandle;
	FURL ClientURL;
	void OnNetOpened(bool bOk, const FMusubiServerEnvelope& Reply);
	void OnClientMsg(const FMusubiServerEnvelope& Env);
	// server
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Sim;
	FDelegateHandle SimHandle;
	TMap<uint64, TWeakObjectPtr<UMusubiNetConnection>> ByConn;
	void OnSimEvent(const FMusubiSimEvent& E);
	UMusubiNetConnection* FindOrOpen(uint64 ConnId, const FMusubiJson& Session, const FMusubiUuid& Entity);
	// packets received this frame, delivered from TickDispatch (game thread, in order)
	TArray<TPair<TWeakObjectPtr<UMusubiNetConnection>, TArray<uint8>>> Inbox;
	void DrainInbox();
};
