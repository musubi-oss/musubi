#pragma once

#include "CoreMinimal.h"
#include "Engine/NetDriver.h"
#include "Engine/NetworkDelegates.h"
#include "MusubiEnvelope.h"
#include "MusubiSimWire.generated.h"
#include "MusubiNetDriver.generated.h"

class UMusubiNetConnection;
class UMusubiSubsystem;
class UMusubiSimulatorSubsystem;

/**
 * Engine networking through the Musubi node (docs/gdk-unreal.md §9.2).
 *
 * Client: `musubi://<world>:0?entity=<uuid>` — `net_open` on the entity, then every engine packet is a
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
class UMusubiAuthorityComponent;

UCLASS(transient, config = Engine)
class MUSUBINET_API UMusubiNetDriver : public UNetDriver, public FNetworkNotify
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

	/** Connection MaxPacket: the node relays up to 1200 bytes, the engine asserts MaxPacket <= MAX_PACKET_SIZE (1024). */
	UPROPERTY(Config)
	int32 MaxRelayPacket = 1024;
	/** Client: `net_rehome` → how long the restarted connection may take to get `NMT_Welcome` before it is a failure. */
	UPROPERTY(Config)
	float RehomeTimeout = 5.f;
	/** Client: after `NMT_Welcome` on a rehome, how long to hold `NMT_Join` for the new owner's NetGUID table (`net_meta`)
	 *  before joining without it (the kept actors are then swapped for the owner's copies instead of reused). A server that
	 *  has just adopted the entity (failover into a replacement) answers late; 1 s lost the identity there. */
	UPROPERTY(Config)
	float GuidTableTimeout = 3.f;

protected:
	/** Client: the entity moved to another simulator. Default: travel to the same URL (`TRAVEL_Absolute`), i.e. a reconnect; projects override for an in-place restart. */
	virtual void OnRehome(const FMusubiServerMessageNetRehome& Msg);
	virtual bool ShouldClientDestroyActor(AActor* Actor, EChannelCloseReason CloseReason) const override;
	int32 Rehomes = 0, Adopted = 0, Reused = 0;
	// FNetworkNotify: owns the control channel during an in-place rehome, forwards everything else to the world
	virtual EAcceptConnection::Type NotifyAcceptingConnection() override { return WorldNotify ? WorldNotify->NotifyAcceptingConnection() : EAcceptConnection::Reject; }
	virtual void NotifyAcceptedConnection(UNetConnection* Connection) override { if (WorldNotify) WorldNotify->NotifyAcceptedConnection(Connection); }
	virtual bool NotifyAcceptingChannel(UChannel* Channel) override { return WorldNotify ? WorldNotify->NotifyAcceptingChannel(Channel) : false; }
	virtual void NotifyControlMessage(UNetConnection* Connection, uint8 MessageType, FInBunch& Bunch) override;

private:
	// client
	TWeakObjectPtr<UMusubiSubsystem> Client;
	FDelegateHandle PacketHandle, CloseHandle, RehomeHandle, MetaHandle;
	FURL ClientURL;
	FNetworkNotify* WorldNotify = nullptr;
	bool bRehoming = false;
	double RehomeStarted = 0;
	TMap<FString, TWeakObjectPtr<AActor>> TornOff;
	// the local player's controller and state kept across the rehome the same way (the new owner announces their GUIDs
	// right after login, kind 2); dropped by the sweep when the new owner's copies came instead
	TWeakObjectPtr<class APlayerController> KeptPC;
	TWeakObjectPtr<class APlayerState> KeptPS;
	// NetGUID pre-registration: the new owner sends `entity → NetGUID (+class)` as `net_meta` on our first packet; registering
	// those GUIDs on the kept actors makes the owner's open bunches re-attach to them (the engine's dormancy path) instead
	// of spawning copies — the same AActor* survives the rehome
	bool bGuidTableSeen = false, bJoinDeferred = false;
	double WelcomeAt = 0;
	FString PendingMap;
	void ApplyGuidTable(const TArray<uint8>& Data);
	void SendJoin(UNetConnection* Connection);
	FDelegateHandle IdentifiedHandle;
	void FinishRehome(bool bOk, const FString& Why);
	void OnEntityIdentified(AActor* Actor, const FString& Id);
	void OnNetOpened(bool bOk, const FMusubiServerEnvelope& Reply);
	/** URL without `?entity=`: `world_join` reply — the account's avatar (found or spawned by the world's player_spawn policy); opens the relay on it. */
	void OnWorldJoined(bool bOk, const FMusubiServerEnvelope& Reply);
	/** `net_open` on `Entity` for the pending client connection. */
	void OpenRelay(const FString& Entity);
	void OnClientMsg(const FMusubiServerEnvelope& Env);
	// server
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Sim;
	FDelegateHandle SimHandle;
	TMap<uint64, TWeakObjectPtr<UMusubiNetConnection>> ByConn;
	void OnSimEvent(const FMusubiSimEvent& E);
	UMusubiNetConnection* FindOrOpen(uint64 ConnId, const FMusubiJson& Session, const FMusubiUuid& Entity);
	/** Server: NetGUIDs of every entity actor (and registered subobject) on this server, sent as `net_meta` before any
	 *  packet reaches the client; `Only` narrows it to one entity (the delta for an entity bound after the connection opened). */
	void SendGuidTable(UMusubiNetConnection* Conn, UMusubiAuthorityComponent* Only = nullptr);
	void OnEntityBound(UMusubiAuthorityComponent* Auth);
	/** Server: the connection's PlayerController/PlayerState NetGUIDs (kind 2), sent once they exist and before their first bunch. */
	void SendPlayerGuids(UMusubiNetConnection* Conn);
	FDelegateHandle BoundHandle;
	// packets received this frame, delivered from TickDispatch (game thread, in order)
	TArray<TPair<TWeakObjectPtr<UMusubiNetConnection>, TArray<uint8>>> Inbox;
	void DrainInbox();
};
