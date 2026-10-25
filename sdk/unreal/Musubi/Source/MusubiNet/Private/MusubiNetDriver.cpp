#include "MusubiNetDriver.h"
#include "Engine/NetworkDelegates.h"
#include "Serialization/MemoryWriter.h"
#include "Serialization/MemoryReader.h"
#include "UObject/UObjectIterator.h"
#include "GameFramework/PlayerController.h"
#include "GameFramework/PlayerState.h"
#include "Engine/PackageMapClient.h"
#include "MusubiAuthorityComponent.h"
#include "Runtime/Launch/Resources/Version.h"
#include "Net/DataChannel.h"
#include "Engine/ActorChannel.h"
#include "Engine/ControlChannel.h"
#include "Engine/LocalPlayer.h"
#include "Misc/NetworkVersion.h"
#include "Misc/Paths.h"

#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/PendingNetGame.h"
#include "Engine/World.h"
#include "IPAddress.h"
#include "MusubiNetConnection.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiSocket.h"
#include "MusubiSubsystem.h"
#include "MusubiWire.generated.h"
#include "PacketHandler.h"
#include "SocketSubsystem.h"
#include "UObject/Package.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiNet, Log, All);

namespace
{
UGameInstance* GameInstanceOf(UNetDriver* Driver)
{
	if (UWorld* W = Driver->GetWorld())
		if (UGameInstance* GI = W->GetGameInstance()) return GI;
	if (GEngine)
	{
		// connecting: the driver belongs to a pending net game, not to a world yet
		if (FWorldContext* C = GEngine->GetWorldContextFromPendingNetGameNetDriver(Driver))
			if (C->OwningGameInstance) return C->OwningGameInstance;
		for (const FWorldContext& C : GEngine->GetWorldContexts())
			if (C.OwningGameInstance) return C.OwningGameInstance;
	}
	return nullptr;
}
} // namespace

UMusubiNetDriver::UMusubiNetDriver(const FObjectInitializer& ObjectInitializer)
	: Super(ObjectInitializer)
{
	if (NetConnectionClassName.IsEmpty()) NetConnectionClassName = TEXT("/Script/MusubiNet.MusubiNetConnection");
	// what [/Script/OnlineSubsystemUtils.IpNetDriver] gives the stock driver and a subclass does not inherit: without
	// these a dedicated server ticks at 1 Hz (GetNetServerMaxTickRate() == 0). A class ini section still overrides.
	SetNetServerMaxTickRate(30);
	MaxNetTickRate = 120;
	MaxClientRate = 100000;
	MaxInternetClientRate = 100000;
	RelevantTimeout = 5.f;
	KeepAliveTime = 0.2f;
	InitialConnectTimeout = 60.f;
	ConnectionTimeout = 60.f;
	SpawnPrioritySeconds = 1.f;
	ServerTravelPause = 4.f;
}

bool UMusubiNetDriver::InitBase(bool bInitAsClient, FNetworkNotify* InNotify, const FURL& URL, bool bReuseAddressAndPort, FString& Error)
{
	return Super::InitBase(bInitAsClient, InNotify, URL, bReuseAddressAndPort, Error);
}

ISocketSubsystem* UMusubiNetDriver::GetSocketSubsystem()
{
	// only for FInternetAddr factories; no socket is ever opened
	return ISocketSubsystem::Get(PLATFORM_SOCKETSUBSYSTEM);
}

// ----- client -----

bool UMusubiNetDriver::InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error)
{
	if (!InitBase(true, InNotify, ConnectURL, false, Error)) return false;
	UGameInstance* GI = GameInstanceOf(this);
	UMusubiSubsystem* Sub = GI ? GI->GetSubsystem<UMusubiSubsystem>() : nullptr;
	TSharedPtr<FMusubiSocket> Sock = Sub ? Sub->Socket() : nullptr;
	if (!Sock.IsValid() || !Sock->IsConnected())
	{
		Error = TEXT("MusubiNet: the Musubi session is not connected (UMusubiSubsystem::Connect first)");
		return false;
	}
	const FString Entity = ConnectURL.GetOption(TEXT("entity="), TEXT(""));
	if (ConnectURL.Host.IsEmpty())
	{
		Error = TEXT("MusubiNet: URL must be musubi://<world>:0[?entity=<uuid>]");
		return false;
	}
	Client = Sub;
	ClientURL = ConnectURL;
	UMusubiNetConnection* Conn = NewObject<UMusubiNetConnection>(GetTransientPackage(), static_cast<UClass*>(NetConnectionClass));
	ServerConnection = Conn;
	Conn->InitLocalConnection(this, nullptr, ConnectURL, USOCK_Pending);
	PacketHandle = Sock->OnTag(EMusubiServerMessageTag::NetPacket).AddUObject(this, &UMusubiNetDriver::OnClientMsg);
	CloseHandle = Sock->OnTag(EMusubiServerMessageTag::NetClose).AddUObject(this, &UMusubiNetDriver::OnClientMsg);
	RehomeHandle = Sock->OnTag(EMusubiServerMessageTag::NetRehome).AddUObject(this, &UMusubiNetDriver::OnClientMsg);
	MetaHandle = Sock->OnTag(EMusubiServerMessageTag::NetMeta).AddUObject(this, &UMusubiNetDriver::OnClientMsg);
	IdentifiedHandle = UMusubiAuthorityComponent::OnClientEntityIdentified.AddUObject(this, &UMusubiNetDriver::OnEntityIdentified);
	CreateInitialClientChannels();
	if (Entity.IsEmpty())
	{
		// no entity in the URL: ask the node for this account's avatar (found, or spawned from
		// the world's player_spawn policy), then open the relay on it
		FMusubiClientMessage Join;
		Join.Tag = EMusubiClientMessageTag::WorldJoin;
		Join.WorldJoin.Emplace();
		Join.WorldJoin->world = ConnectURL.Host;
		Sock->Request(Join, FMusubiReply::CreateUObject(this, &UMusubiNetDriver::OnWorldJoined));
		UE_LOG(LogMusubiNet, Log, TEXT("connecting through the node: world=%s, asking for my avatar"), *ConnectURL.Host);
		return true;
	}
	OpenRelay(Entity);
	return true;
}

void UMusubiNetDriver::OpenRelay(const FString& Entity)
{
	UMusubiNetConnection* Conn = Cast<UMusubiNetConnection>(ServerConnection);
	TSharedPtr<FMusubiSocket> Sock = Client.IsValid() ? Client->Socket() : nullptr;
	if (!Conn || !Sock.IsValid()) return;
	Conn->Entity = Entity;
	FMusubiClientMessage Open;
	Open.Tag = EMusubiClientMessageTag::NetOpen;
	Open.NetOpen.Emplace();
	Open.NetOpen->world = ClientURL.Host;
	Open.NetOpen->entity = Entity;
	Sock->Request(Open, FMusubiReply::CreateUObject(this, &UMusubiNetDriver::OnNetOpened));
	UE_LOG(LogMusubiNet, Log, TEXT("connecting through the node: world=%s entity=%s"), *ClientURL.Host, *Entity);
}

void UMusubiNetDriver::OnWorldJoined(bool bOk, const FMusubiServerEnvelope& Reply)
{
	if (!Cast<UMusubiNetConnection>(ServerConnection)) return;
	if (!bOk || !Reply.Msg.WorldJoined.IsSet())
	{
		const FString Why = Reply.Msg.Error.IsSet() ? Reply.Msg.Error->message : TEXT("world_join failed");
		UE_LOG(LogMusubiNet, Error, TEXT("world_join refused: %s"), *Why);
		if (GEngine) GEngine->BroadcastNetworkFailure(GetWorld(), this, ENetworkFailure::ConnectionLost, Why);
		return;
	}
	const FMusubiServerMessageWorldJoined& J = *Reply.Msg.WorldJoined;
	const FString Entity = J.entity.ToString();
	ClientURL.AddOption(*FString::Printf(TEXT("entity=%s"), *Entity));
	UE_LOG(LogMusubiNet, Log, TEXT("my avatar in %s is %s (%s) at %.1f,%.1f"), *J.world, *Entity, J.spawned ? TEXT("spawned") : TEXT("found"), J.x, J.y);
	OpenRelay(Entity);
}

void UMusubiNetDriver::OnNetOpened(bool bOk, const FMusubiServerEnvelope& Reply)
{
	UMusubiNetConnection* Conn = Cast<UMusubiNetConnection>(ServerConnection);
	if (!Conn) return;
	if (!bOk || !Reply.Msg.NetOpened.IsSet())
	{
		const FString Why = Reply.Msg.Error.IsSet() ? Reply.Msg.Error->message : TEXT("net_open failed");
		UE_LOG(LogMusubiNet, Error, TEXT("net_open refused: %s"), *Why);
		if (GEngine) GEngine->BroadcastNetworkFailure(GetWorld(), this, ENetworkFailure::ConnectionLost, Why);
		return;
	}
	const FMusubiServerMessageNetOpened& O = *Reply.Msg.NetOpened;
	UE_LOG(LogMusubiNet, Log, TEXT("relay %llu open on %s (owner %s, max_packet %u)"), (unsigned long long)O.conn, *O.entity,
		O.server.IsSet() ? *O.server->ToString() : TEXT("in transit"), O.max_packet);
	Conn->Opened(O.conn, (int32)O.max_packet);
}

void UMusubiNetDriver::OnClientMsg(const FMusubiServerEnvelope& Env)
{
	UMusubiNetConnection* Conn = Cast<UMusubiNetConnection>(ServerConnection);
	if (!Conn || !Conn->IsOpen()) return;
	if (Env.Msg.NetPacket.IsSet())
	{
		if (Env.Msg.NetPacket->conn == Conn->ConnId) Inbox.Emplace(Conn, Env.Msg.NetPacket->data);
	}
	else if (Env.Msg.NetClose.IsSet())
	{
		if (Env.Msg.NetClose->conn == Conn->ConnId)
		{
			UE_LOG(LogMusubiNet, Log, TEXT("server closed relay %llu: %s"), (unsigned long long)Conn->ConnId, *Env.Msg.NetClose->reason);
			Conn->Close();
		}
	}
	else if (Env.Msg.NetRehome.IsSet())
	{
		if (Env.Msg.NetRehome->conn == Conn->ConnId) OnRehome(*Env.Msg.NetRehome);
	}
	else if (Env.Msg.NetMeta.IsSet())
	{
		if (Env.Msg.NetMeta->conn == Conn->ConnId) ApplyGuidTable(Env.Msg.NetMeta->data);
	}
}

// net_meta kind 1: u8 kind, u32 count, count × (FString entity, FNetworkGUID (its own FArchive operator), FString class path)
static constexpr uint8 kMetaGuidTable = 1;
// net_meta kind 2: u8 kind, u32 count, count × (FString "pc"|"ps", FNetworkGUID, FString class path) — the player's own actors
static constexpr uint8 kMetaPlayerGuids = 2;

void UMusubiNetDriver::ApplyGuidTable(const TArray<uint8>& Data)
{
	FMemoryReader Ar(Data);
	uint8 Kind = 0; Ar << Kind;
	if (Ar.IsError() || (Kind != kMetaGuidTable && Kind != kMetaPlayerGuids)) { UE_LOG(LogMusubiNet, Warning, TEXT("net_meta kind %d ignored"), Kind); return; }
	uint32 Count = 0; Ar << Count;
	if (Kind == kMetaPlayerGuids)
	{
		int32 Done = 0;
		for (uint32 i = 0; i < Count && !Ar.IsError(); ++i)
		{
			FString What, ClassPath; FNetworkGUID G;
			Ar << What; Ar << G; Ar << ClassPath;
			if (!G.IsValid() || !G.IsDynamic()) continue;
			AActor* A = What == TEXT("pc") ? static_cast<AActor*>(KeptPC.Get()) : What == TEXT("ps") ? static_cast<AActor*>(KeptPS.Get()) : nullptr;
			if (!A || A->GetClass()->GetPathName() != ClassPath)
			{
				UE_LOG(LogMusubiNet, Log, TEXT("rehome: player GUID row %s (%s) not applied: kept actor %s"), *What, *ClassPath, A ? *A->GetClass()->GetPathName() : TEXT("gone"));
				continue;
			}
			GuidCache->RegisterNetGUID_Client(G, A);
			++Done;
		}
		UE_LOG(LogMusubiNet, Log, TEXT("rehome: player GUIDs from the new owner: %u rows, %d kept (controller/state) pre-registered"), Count, Done);
		return;
	}
	int32 Registered = 0, ClassMismatch = 0, SubRegistered = 0;
	TSet<UObject*> Taken; // subobjects already matched in this table (one per class per actor)
	for (uint32 i = 0; i < Count && !Ar.IsError(); ++i)
	{
		FString Id, ClassPath; FNetworkGUID G;
		Ar << Id; Ar << G; Ar << ClassPath;
		if (!G.IsValid() || !G.IsDynamic()) continue;
		FString Entity = Id, Sub;
		Id.Split(TEXT("/"), &Entity, &Sub);
		TWeakObjectPtr<AActor>* P = TornOff.Find(Entity);
		AActor* A = P ? P->Get() : nullptr;
		if (!A) continue;
		if (Sub.IsEmpty())
		{
			if (A->GetClass()->GetPathName() != ClassPath) { ++ClassMismatch; continue; } // the owner's copy will replace it (swap path)
			GuidCache->RegisterNetGUID_Client(G, A);
			++Registered;
			continue;
		}
		// a registered subobject: the owner's name means nothing here (ours was created by replication under a generated
		// name) — match by class within the kept actor's registry, one per class
		const UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>();
		FString SubClass = ClassPath; { FString L, R; if (ClassPath.Split(TEXT("|"), &L, &R)) SubClass = R; }
		for (const TWeakObjectPtr<UObject>& W : Auth ? Auth->MusubiSubObjects() : TArray<TWeakObjectPtr<UObject>>())
			if (UObject* O = W.Get())
				if (!Taken.Contains(O) && O->GetClass()->GetPathName() == SubClass)
				{
					GuidCache->RegisterNetGUID_Client(G, O);
					Taken.Add(O);
					++SubRegistered;
					break;
				}
	}
	bGuidTableSeen = true;
	UE_LOG(LogMusubiNet, Log, TEXT("rehome: NetGUID table from the new owner: %u rows, %d kept actors pre-registered, %d subobjects, %d class mismatches"), Count, Registered, SubRegistered, ClassMismatch);
	if (bJoinDeferred && ServerConnection) SendJoin(ServerConnection);
}

void UMusubiNetDriver::SendJoin(UNetConnection* Connection)
{
	bJoinDeferred = false;
	FNetControlMessage<NMT_Netspeed>::Send(Connection, Connection->CurrentNetSpeed);
	FNetControlMessage<NMT_Join>::Send(Connection);
	Connection->FlushNet(true);
	FinishRehome(true, PendingMap);
}

void UMusubiNetDriver::OnRehome(const FMusubiServerMessageNetRehome& Msg)
{
	UMusubiNetConnection* Old = Cast<UMusubiNetConnection>(ServerConnection);
	UWorld* W = GetWorld();
	UE_LOG(LogMusubiNet, Log, TEXT("entity %s moved to %s: rehoming in place"), *Msg.entity, Msg.server.IsSet() ? *Msg.server->ToString() : TEXT("another node"));
	if (!Old || !W || bRehoming || !Old->IsOpen())
	{
		FinishRehome(false, TEXT("no open connection to rehome"));
		return;
	}
	++Rehomes;
	bRehoming = true;
	RehomeStarted = FPlatformTime::Seconds();
	// 1. keep the world: actors that are Musubi entities stay (closed as for dormancy, so they are neither destroyed nor
	//    torn off). The new owner sends its NetGUID table (net_meta) before its first bunch: GUIDs registered on the kept
	//    actors make its open bunches re-attach to them (same AActor*). An entity missing from the table, or of another
	//    class there, gets the owner's copy and OnEntityIdentified swaps the old one out — no empty frame either way
	TornOff.Reset();
	bGuidTableSeen = false; bJoinDeferred = false; PendingMap.Empty();
	APlayerController* LocalPC = W->GetFirstPlayerController();
	APlayerState* LocalPS = LocalPC ? LocalPC->PlayerState.Get() : nullptr;
	KeptPC = LocalPC; KeptPS = LocalPS;
	for (UChannel* Ch : TArray<UChannel*>(Old->OpenChannels))
	{
		UActorChannel* AC = Cast<UActorChannel>(Ch);
		if (!AC || !AC->Actor) continue;
		const UMusubiAuthorityComponent* Auth = AC->Actor->FindComponentByClass<UMusubiAuthorityComponent>();
		const bool bPlayer = AC->Actor == LocalPC || (LocalPS && AC->Actor == LocalPS);
		if (!bPlayer && (!Auth || Auth->NetEntityId.IsEmpty())) continue;
		if (!bPlayer) TornOff.Add(Auth->NetEntityId, AC->Actor);
		// close as for dormancy: the engine keeps the actor (ClientSetActorDormant) without tearing it off, so the new
		// owner's open bunch can re-attach to it; `Dormant` is normally set by a received close bunch, we close locally
		AC->Dormant = true;
		AC->ConditionalCleanUp(true, EChannelCloseReason::Dormancy);
	}
	// 2. a fresh engine connection on the same relay conn (the node already routes it to the new owner; the server side
	//    opens its UMusubiNetConnection on our first packet)
	const uint64 ConnId = Old->ConnId;
	const int32 OldMaxPacket = Old->MaxPacket;
	Old->bKeepRelay = true;
	// UNetConnection::CleanUp nulls Driver->ServerConnection itself when it is this connection; with it already null the
	// engine takes the old connection for a client one and asserts in RemoveClientConnection
	check(ServerConnection == Old);
	// CleanUp → DestroyOwningActor → APlayerController::OnNetCleanup would destroy the kept controller: detach it first
	// (HandleClientPlayer re-links Player/NetConnection when the new owner's bunch re-attaches)
	Old->OwningActor = nullptr;
	Old->PlayerController = nullptr;
	Old->CleanUp();
	check(ServerConnection == nullptr);
	// the new owner numbers its NetGUIDs independently of the old one: with the old cache its GUIDs resolve to our old
	// actors ("open bunch for a torn off actor") — start the cache over
	GuidCache = TSharedPtr<FNetGUIDCache>(new FNetGUIDCache(this));
	UMusubiNetConnection* Conn = NewObject<UMusubiNetConnection>(GetTransientPackage(), static_cast<UClass*>(NetConnectionClass));
	ServerConnection = Conn;
	Conn->InitLocalConnection(this, nullptr, ClientURL, USOCK_Pending);
	Conn->Opened(ConnId, OldMaxPacket);
	CreateInitialClientChannels();
	WorldNotify = Notify;
	Notify = this;
	// 3. the login handshake the engine normally runs in UPendingNetGame, without the map load: the relay PacketHandler
	// completes its handshake synchronously, then NMT_Hello carries the travel URL's EncryptionToken again so the new
	// owner re-keys the connection (NMT_EncryptionAck below) the same way the first owner did
	if (Conn->Handler.IsValid()) Conn->Handler->BeginHandshaking();
	uint8 IsLittleEndian = uint8(PLATFORM_LITTLE_ENDIAN);
	uint32 LocalNetworkVersion = FNetworkVersion::GetLocalNetworkVersion();
	FString EncryptionToken = ClientURL.GetOption(TEXT("EncryptionToken="), TEXT(""));
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION <= 3
	uint16 LocalNetworkFeatures = static_cast<uint16>(GetNetworkRuntimeFeatures()); // NMT_Hello carries a uint16 in 5.3
#else
	EEngineNetworkRuntimeFeatures LocalNetworkFeatures = GetNetworkRuntimeFeatures();
#endif
	FNetControlMessage<NMT_Hello>::Send(Conn, IsLittleEndian, LocalNetworkVersion, EncryptionToken, LocalNetworkFeatures);
	Conn->FlushNet();
}

void UMusubiNetDriver::NotifyControlMessage(UNetConnection* Connection, uint8 MessageType, FInBunch& Bunch)
{
	if (!bRehoming || Connection != ServerConnection)
	{
		if (WorldNotify) WorldNotify->NotifyControlMessage(Connection, MessageType, Bunch);
		return;
	}
	switch (MessageType)
	{
	case NMT_Challenge:
		if (FNetControlMessage<NMT_Challenge>::Receive(Bunch, Connection->Challenge))
		{
			FURL PartialURL(ClientURL);
			PartialURL.Host.Empty();
			PartialURL.Port = PartialURL.UrlConfig.DefaultPort;
			PartialURL.Map.Empty();
			UGameInstance* GI = GameInstanceOf(this);
			if (ULocalPlayer* LP = GI ? GI->GetFirstGamePlayer() : nullptr) Connection->PlayerId = LP->GetPreferredUniqueNetId();
			FString OnlinePlatformNameString = GI ? GI->GetOnlinePlatformName().ToString() : FString();
			Connection->ClientResponse = TEXT("0");
			FString URLString(PartialURL.ToString());
			FNetControlMessage<NMT_Login>::Send(Connection, Connection->ClientResponse, URLString, Connection->PlayerId, OnlinePlatformNameString);
			Connection->FlushNet();
		}
		break;
	case NMT_Welcome:
	{
		FString Map, GameName, RedirectURL;
		if (!FNetControlMessage<NMT_Welcome>::Receive(Bunch, Map, GameName, RedirectURL)) break;
		FURL Default;
		const FURL Level(&Default, *Map, TRAVEL_Partial);
		if (!Level.Map.IsEmpty() && GetWorld() && FPaths::GetBaseFilename(Level.Map) != GetWorld()->GetMapName())
		{
			FinishRehome(false, FString::Printf(TEXT("new owner runs level %s, we are in %s"), *Level.Map, *GetWorld()->GetMapName()));
			break;
		}
		PendingMap = Map;
		if (bGuidTableSeen || TornOff.Num() == 0) SendJoin(Connection);
		else { bJoinDeferred = true; WelcomeAt = FPlatformTime::Seconds(); } // join only once the owner's NetGUID table is in
		break;
	}
	case NMT_Failure:
	{
		FString Why;
		if (!FNetControlMessage<NMT_Failure>::Receive(Bunch, Why)) Why.Empty();
		FinishRehome(false, Why.IsEmpty() ? TEXT("NMT_Failure") : Why);
		break;
	}
	case NMT_Upgrade:
		FinishRehome(false, TEXT("NMT_Upgrade"));
		break;
	case NMT_EncryptionAck:
		// as UPendingNetGame::NotifyControlMessage: the game's key provider answers, the connection switches to the new key
		if (FNetDelegates::OnReceivedNetworkEncryptionAck.IsBound())
		{
			TWeakObjectPtr<UNetConnection> Weak(Connection);
			FNetDelegates::OnReceivedNetworkEncryptionAck.Execute(FOnEncryptionKeyResponse::CreateLambda([Weak](const FEncryptionKeyResponse& Response)
			{
				UNetConnection* C = Weak.Get();
				if (!C || C->GetConnectionState() == USOCK_Closed) return;
				if (Response.Response == EEncryptionResponse::Success) C->EnableEncryption(Response.EncryptionData);
				else UE_LOG(LogMusubiNet, Warning, TEXT("rehome: encryption key provider failed [%s] %s"), LexToString(Response.Response), *Response.ErrorMsg);
			}));
		}
		else FinishRehome(false, TEXT("NMT_EncryptionAck without a key provider (FNetDelegates::OnReceivedNetworkEncryptionAck)"));
		break;
	default:
		if (WorldNotify) WorldNotify->NotifyControlMessage(Connection, MessageType, Bunch);
	}
}

void UMusubiNetDriver::FinishRehome(bool bOk, const FString& Why)
{
	const bool bWas = bRehoming;
	bRehoming = false;
	if (WorldNotify) { Notify = WorldNotify; WorldNotify = nullptr; }
	if (bOk)
	{
		UE_LOG(LogMusubiNet, Log, TEXT("rehomed in place (%s), %d actors kept for adoption"), *Why, TornOff.Num());
		return;
	}
	UE_LOG(LogMusubiNet, Warning, TEXT("in-place rehome failed (%s)%s"), *Why, bWas ? TEXT(": travelling") : TEXT(": reconnecting"));
	TornOff.Reset();
	if (GEngine)
		if (UWorld* W = GetWorld()) GEngine->SetClientTravel(W, *ClientURL.ToString(), TRAVEL_Absolute);
}

void UMusubiNetDriver::OnEntityIdentified(AActor* Actor, const FString& Id)
{
	TWeakObjectPtr<AActor>* P = TornOff.Find(Id);
	if (!P) return;
	AActor* Twin = P->Get();
	TornOff.Remove(Id);
	if (Twin && Twin != Actor)
	{
		++Adopted;
		UE_LOG(LogMusubiNet, Log, TEXT("rehome: swapped %s for %s (entity %s)"), *Twin->GetName(), *Actor->GetName(), *Id);
		// a client may not Destroy() a replicated actor it does not own (DestroyActor refuses without authority or tear-off)
		if (UWorld* W = Twin->GetWorld()) W->DestroyActor(Twin, true);
	}
}

// ----- server -----

bool UMusubiNetDriver::InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error)
{
	if (!InitBase(false, InNotify, LocalURL, bReuseAddressAndPort, Error)) return false;
	UGameInstance* GI = GameInstanceOf(this);
	UMusubiSimulatorSubsystem* S = GI ? GI->GetSubsystem<UMusubiSimulatorSubsystem>() : nullptr;
	if (!S)
	{
		Error = TEXT("MusubiNet: no UMusubiSimulatorSubsystem (dedicated server with Musubi simulator settings required)");
		return false;
	}
	Sim = S;
	SimHandle = S->OnSimEvent.AddUObject(this, &UMusubiNetDriver::OnSimEvent);
	// no socket, no stateless-connect challenge: the node authenticated the session already
	ServerConnection = nullptr;
	UE_LOG(LogMusubiNet, Log, TEXT("%s listening through the node (simulator stream)"), *GetDescription());
	BoundHandle = UMusubiAuthorityComponent::OnServerEntityBound.AddUObject(this, &UMusubiNetDriver::OnEntityBound);
	return true;
}

void UMusubiNetDriver::OnEntityBound(UMusubiAuthorityComponent* Auth)
{
	// an entity that reached this server after a client's connection opened: its rows go out before its first bunch
	if (!Auth || Auth->GetOwner() == nullptr || Auth->GetOwner()->GetWorld() != GetWorld()) return;
	for (TPair<uint64, TWeakObjectPtr<UMusubiNetConnection>>& KV : ByConn)
		if (UMusubiNetConnection* C = KV.Value.Get())
			if (C->GetConnectionState() != USOCK_Closed) SendGuidTable(C, Auth);
}

UMusubiNetConnection* UMusubiNetDriver::FindOrOpen(uint64 ConnId, const FMusubiJson& Session, const FMusubiUuid& Entity)
{
	if (TWeakObjectPtr<UMusubiNetConnection>* P = ByConn.Find(ConnId))
	{
		if (UMusubiNetConnection* C = P->Get())
			if (C->GetConnectionState() != USOCK_Closed) return C;
		ByConn.Remove(ConnId);
	}
	UMusubiNetConnection* Conn = NewObject<UMusubiNetConnection>(GetTransientPackage(), static_cast<UClass*>(NetConnectionClass));
	// a synthetic address so the engine's per-address bookkeeping/logging has something to key on
	TSharedRef<FInternetAddr> Addr = GetSocketSubsystem()->CreateInternetAddr();
	Addr->SetIp((uint32)(ConnId & 0xffffffffu));
	Addr->SetPort((int32)((ConnId >> 32) & 0xffffu));
	Conn->ConnId = ConnId;
	Conn->Session = Session;
	Conn->Entity = Entity.ToString();
	// the entity rides on the connection URL: a client that joined without `?entity=` (world_join)
	// never put it in its travel URL, so the game mode reads it from here instead of the login options
	FURL RelayURL;
	RelayURL.AddOption(*FString::Printf(TEXT("entity=%s"), *Conn->Entity));
	Conn->InitRemoteConnection(this, nullptr, RelayURL, *Addr, USOCK_Open);
	AddClientConnection(Conn);
	if (Conn->Handler.IsValid()) Conn->Handler->BeginHandshaking();
	ByConn.Add(ConnId, Conn);
	UE_LOG(LogMusubiNet, Log, TEXT("relay %llu opened by first packet (entity %s)"), (unsigned long long)ConnId, *Conn->Entity);
	SendGuidTable(Conn);
	return Conn;
}

void UMusubiNetDriver::SendPlayerGuids(UMusubiNetConnection* Conn)
{
	UMusubiSimulatorSubsystem* S = Sim.Get();
	if (!S || !GuidCache.IsValid() || !Conn->PlayerController) return;
	Conn->bPlayerAnnounced = true;
	TArray<uint8> Bytes;
	FMemoryWriter Ar(Bytes);
	uint8 Kind = kMetaPlayerGuids; Ar << Kind;
	uint32 Count = 0; const int64 CountPos = Ar.Tell(); Ar << Count;
	auto Row = [&](const TCHAR* What, AActor* A)
	{
		if (!A) return;
		FNetworkGUID G = GuidCache->GetOrAssignNetGUID(A);
		if (!G.IsValid()) return;
		FString W = What, Cls = A->GetClass()->GetPathName();
		Ar << W; Ar << G; Ar << Cls;
		++Count;
	};
	Row(TEXT("pc"), Conn->PlayerController);
	Row(TEXT("ps"), Conn->PlayerController->PlayerState.Get());
	const int64 End = Ar.Tell(); Ar.Seek(CountPos); Ar << Count; Ar.Seek(End);
	UE_LOG(LogMusubiNet, Log, TEXT("relay %llu: player GUIDs sent (%u rows)"), (unsigned long long)Conn->ConnId, Count);
	S->NetSendMeta(Conn->ConnId, Conn->Session, MoveTemp(Bytes));
}

void UMusubiNetDriver::SendGuidTable(UMusubiNetConnection* Conn, UMusubiAuthorityComponent* Only)
{
	UMusubiSimulatorSubsystem* S = Sim.Get();
	UWorld* W = GetWorld();
	if (!S || !W || !GuidCache.IsValid()) return;
	TArray<uint8> Bytes;
	FMemoryWriter Ar(Bytes);
	uint8 Kind = kMetaGuidTable; Ar << Kind;
	uint32 Count = 0; const int64 CountPos = Ar.Tell(); Ar << Count;
	auto Rows = [&](UMusubiAuthorityComponent* Auth)
	{
		AActor* A = Auth->GetOwner();
		if (!A || !IsValid(A) || A->GetWorld() != W || Auth->NetEntityId.IsEmpty()) return;
		FNetworkGUID G = GuidCache->GetOrAssignNetGUID(A);
		if (!G.IsValid()) return;
		FString Entity = Auth->NetEntityId, ClassPath = A->GetClass()->GetPathName();
		Ar << Entity; Ar << G; Ar << ClassPath;
		++Count;
		// registered subobjects (RegisterMusubiSubObject): `entity/Name`, class = actor class | subobject class
		for (const TWeakObjectPtr<UObject>& Wk : Auth->MusubiSubObjects())
			if (UObject* O = Wk.Get())
			{
				FNetworkGUID SG = GuidCache->GetOrAssignNetGUID(O);
				if (!SG.IsValid()) continue;
				FString Id = Entity + TEXT("/") + O->GetName(), Cls = ClassPath + TEXT("|") + O->GetClass()->GetPathName();
				Ar << Id; Ar << SG; Ar << Cls;
				++Count;
			}
	};
	if (Only) Rows(Only);
	else for (TObjectIterator<UMusubiAuthorityComponent> It; It; ++It) Rows(*It);
	if (Count == 0) return;
	const int64 End = Ar.Tell(); Ar.Seek(CountPos); Ar << Count; Ar.Seek(End);
	UE_LOG(LogMusubiNet, Log, TEXT("relay %llu: NetGUID table %s, %u rows, %d bytes"), (unsigned long long)Conn->ConnId, Only ? TEXT("delta") : TEXT("sent"), Count, Bytes.Num());
	S->NetSendMeta(Conn->ConnId, Conn->Session, MoveTemp(Bytes));
}

void UMusubiNetDriver::OnSimEvent(const FMusubiSimEvent& E)
{
	if (E.Tag == EMusubiSimEventTag::NetPacket && E.NetPacket.IsSet())
	{
		const FMusubiSimEventNetPacket& P = *E.NetPacket;
		if (UMusubiNetConnection* Conn = FindOrOpen(P.conn, P.session, P.entity)) Inbox.Emplace(Conn, P.data);
	}
	else if (E.Tag == EMusubiSimEventTag::NetClose && E.NetClose.IsSet())
	{
		if (TWeakObjectPtr<UMusubiNetConnection>* P = ByConn.Find(E.NetClose->conn))
		{
			if (UMusubiNetConnection* C = P->Get())
			{
				UE_LOG(LogMusubiNet, Log, TEXT("relay %llu closed by the node: %s"), (unsigned long long)E.NetClose->conn, *E.NetClose->reason);
				C->Close();
			}
			ByConn.Remove(E.NetClose->conn);
		}
	}
}

// ----- both -----

void UMusubiNetDriver::SendPacket(UMusubiNetConnection* Conn, const uint8* Data, int32 Count)
{
	if (!Conn || !Conn->IsOpen() || Count <= 0) return;
	if (IsServer())
	{
		if (UMusubiSimulatorSubsystem* S = Sim.Get()) S->NetSendPacket(Conn->ConnId, Conn->Session, TArray<uint8>(Data, Count));
		return;
	}
	UMusubiSubsystem* Sub = Client.Get();
	TSharedPtr<FMusubiSocket> Sock = Sub ? Sub->Socket() : nullptr;
	if (!Sock.IsValid()) return;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::NetPacket;
	M.NetPacket.Emplace();
	M.NetPacket->world = Conn->World;
	M.NetPacket->entity = Conn->Entity;
	M.NetPacket->conn = Conn->ConnId;
	M.NetPacket->data = TArray<uint8>(Data, Count);
	Sock->Send(M);
	UE_LOG(LogMusubiNet, Verbose, TEXT("-> net_packet %d bytes on relay %llu"), Count, (unsigned long long)Conn->ConnId);
}

void UMusubiNetDriver::DrainInbox()
{
	TArray<TPair<TWeakObjectPtr<UMusubiNetConnection>, TArray<uint8>>> Now = MoveTemp(Inbox);
	Inbox.Reset();
	for (TPair<TWeakObjectPtr<UMusubiNetConnection>, TArray<uint8>>& P : Now)
		if (UMusubiNetConnection* C = P.Key.Get())
			if (C->GetConnectionState() != USOCK_Closed && P.Value.Num() > 0) C->ReceivedRawPacket(P.Value.GetData(), P.Value.Num());
}

void UMusubiNetDriver::TickDispatch(float DeltaTime)
{
	if (bJoinDeferred && ServerConnection && FPlatformTime::Seconds() - WelcomeAt > GuidTableTimeout)
	{
		UE_LOG(LogMusubiNet, Warning, TEXT("rehome: no NetGUID table from the new owner within %.1fs, joining without it (kept actors will be swapped)"), GuidTableTimeout);
		SendJoin(ServerConnection);
	}
	if (bRehoming && FPlatformTime::Seconds() - RehomeStarted > RehomeTimeout) FinishRehome(false, TEXT("timeout"));
	else if (!bRehoming && TornOff.Num() && FPlatformTime::Seconds() - RehomeStarted > 2.0 * RehomeTimeout)
	{
		// kept actors the new owner re-attached to (pre-registered GUID) stay; entities it never sent again (left our
		// interest during the move) are stale twins: drop them
		int32 Kept = 0, Dropped = 0;
		for (TPair<FString, TWeakObjectPtr<AActor>>& KV : TornOff)
			if (AActor* A = KV.Value.Get())
			{
				if (ServerConnection && ServerConnection->FindActorChannelRef(A))
				{
					++Kept; ++Reused;
					UE_LOG(LogMusubiNet, Log, TEXT("rehome: reused %s in place (entity %s)"), *A->GetName(), *KV.Key);
				}
				else { ++Dropped; if (UWorld* AW = A->GetWorld()) AW->DestroyActor(A, true); }
			}
		for (AActor* A : { static_cast<AActor*>(KeptPC.Get()), static_cast<AActor*>(KeptPS.Get()) })
			if (A)
			{
				const bool bOn = ServerConnection && ServerConnection->FindActorChannelRef(A);
				UE_LOG(LogMusubiNet, Log, TEXT("rehome: %s %s"), bOn ? TEXT("reused player actor") : TEXT("dropped stale player actor"), *A->GetName());
				if (!bOn) if (UWorld* AW = A->GetWorld()) AW->DestroyActor(A, true);
			}
		KeptPC.Reset(); KeptPS.Reset();
		UE_LOG(LogMusubiNet, Log, TEXT("rehome: %d actors swapped for the new owner's, %d reused in place, %d dropped"), Adopted, Kept, Dropped);
		TornOff.Reset();
	}
	Super::TickDispatch(DeltaTime);
	DrainInbox();
	if (!ServerConnection)
		for (TPair<uint64, TWeakObjectPtr<UMusubiNetConnection>>& KV : ByConn)
			if (UMusubiNetConnection* C = KV.Value.Get())
				if (!C->bPlayerAnnounced && C->PlayerController && C->GetConnectionState() != USOCK_Closed) SendPlayerGuids(C);
}

void UMusubiNetDriver::LowLevelSend(TSharedPtr<const FInternetAddr> Address, void* Data, int32 CountBits, FOutPacketTraits& Traits)
{
	// connectionless sends (stateless handshake, beacons by address) have no meaning on a relay
	UE_LOG(LogMusubiNet, Verbose, TEXT("LowLevelSend by address ignored (%d bits)"), CountBits);
}

FString UMusubiNetDriver::LowLevelGetNetworkNumber()
{
	return TEXT("musubi");
}

bool UMusubiNetDriver::IsNetResourceValid()
{
	if (IsServer()) return Sim.IsValid();
	UMusubiSubsystem* Sub = Client.Get();
	return Sub && Sub->Socket().IsValid();
}

void UMusubiNetDriver::LowLevelDestroy()
{
	if (BoundHandle.IsValid()) { UMusubiAuthorityComponent::OnServerEntityBound.Remove(BoundHandle); BoundHandle.Reset(); }
	if (UMusubiSubsystem* Sub = Client.Get())
		if (TSharedPtr<FMusubiSocket> Sock = Sub->Socket())
		{
			Sock->OnTag(EMusubiServerMessageTag::NetPacket).Remove(PacketHandle);
			Sock->OnTag(EMusubiServerMessageTag::NetClose).Remove(CloseHandle);
			Sock->OnTag(EMusubiServerMessageTag::NetRehome).Remove(RehomeHandle);
			Sock->OnTag(EMusubiServerMessageTag::NetMeta).Remove(MetaHandle);
			UMusubiAuthorityComponent::OnClientEntityIdentified.Remove(IdentifiedHandle);
			if (UMusubiNetConnection* Conn = Cast<UMusubiNetConnection>(ServerConnection))
				if (Conn->IsOpen())
				{
					FMusubiClientMessage M;
					M.Tag = EMusubiClientMessageTag::NetClose;
					M.NetClose.Emplace();
					M.NetClose->world = Conn->World;
					M.NetClose->entity = Conn->Entity;
					M.NetClose->conn = Conn->ConnId;
					M.NetClose->reason = TEXT("client closed");
					Sock->Send(M);
				}
		}
	if (UMusubiSimulatorSubsystem* S = Sim.Get())
	{
		S->OnSimEvent.Remove(SimHandle);
		for (const TPair<uint64, TWeakObjectPtr<UMusubiNetConnection>>& P : ByConn)
			if (UMusubiNetConnection* C = P.Value.Get())
			{
				FMusubiUuid Id;
				FMusubiUuid::Parse(C->Entity, Id);
				S->NetClose(C->ConnId, C->Session, Id, TEXT("server shutdown"));
			}
	}
	ByConn.Reset();
	Inbox.Reset();
	Super::LowLevelDestroy();
}
