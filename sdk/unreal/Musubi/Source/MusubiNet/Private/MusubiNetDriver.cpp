#include "MusubiNetDriver.h"
#include "MusubiAuthorityComponent.h"
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
	if (ConnectURL.Host.IsEmpty() || Entity.IsEmpty())
	{
		Error = TEXT("MusubiNet: URL must be musubi://<world>?entity=<uuid>");
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
	IdentifiedHandle = UMusubiAuthorityComponent::OnClientEntityIdentified.AddUObject(this, &UMusubiNetDriver::OnEntityIdentified);
	FMusubiClientMessage Open;
	Open.Tag = EMusubiClientMessageTag::NetOpen;
	Open.NetOpen.Emplace();
	Open.NetOpen->world = ConnectURL.Host;
	Open.NetOpen->entity = Entity;
	Sock->Request(Open, FMusubiReply::CreateUObject(this, &UMusubiNetDriver::OnNetOpened));
	CreateInitialClientChannels();
	UE_LOG(LogMusubiNet, Log, TEXT("connecting through the node: world=%s entity=%s"), *ConnectURL.Host, *Entity);
	return true;
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
	// 1. keep the world: actors that are Musubi entities stay as torn-off actors (the engine sets bTearOff itself in
	//    UActorChannel::CleanUp(TearOff) and keeps the actor since ShouldClientDestroyTearOffActors() is false); the new
	//    owner's replication re-creates them and OnEntityIdentified swaps the twin out — no empty frame in between
	TornOff.Reset();
	for (UChannel* Ch : TArray<UChannel*>(Old->OpenChannels))
	{
		UActorChannel* AC = Cast<UActorChannel>(Ch);
		if (!AC || !AC->Actor) continue;
		const UMusubiAuthorityComponent* Auth = AC->Actor->FindComponentByClass<UMusubiAuthorityComponent>();
		if (!Auth || Auth->NetEntityId.IsEmpty()) continue;
		TornOff.Add(Auth->NetEntityId, AC->Actor);
		AC->ConditionalCleanUp(true, EChannelCloseReason::TearOff);
	}
	// 2. a fresh engine connection on the same relay conn (the node already routes it to the new owner; the server side
	//    opens its UMusubiNetConnection on our first packet)
	const uint64 ConnId = Old->ConnId;
	const int32 OldMaxPacket = Old->MaxPacket;
	Old->bKeepRelay = true;
	ServerConnection = nullptr; // UNetConnection::CleanUp asserts Driver->ServerConnection == this when it is set
	Old->CleanUp();
	UMusubiNetConnection* Conn = NewObject<UMusubiNetConnection>(GetTransientPackage(), static_cast<UClass*>(NetConnectionClass));
	ServerConnection = Conn;
	Conn->InitLocalConnection(this, nullptr, ClientURL, USOCK_Pending);
	Conn->Opened(ConnId, OldMaxPacket);
	CreateInitialClientChannels();
	WorldNotify = Notify;
	Notify = this;
	// 3. the login handshake the engine normally runs in UPendingNetGame, without the map load
	uint8 IsLittleEndian = uint8(PLATFORM_LITTLE_ENDIAN);
	uint32 LocalNetworkVersion = FNetworkVersion::GetLocalNetworkVersion();
	FString EncryptionToken;
	EEngineNetworkRuntimeFeatures LocalNetworkFeatures = GetNetworkRuntimeFeatures();
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
		FNetControlMessage<NMT_Netspeed>::Send(Connection, Connection->CurrentNetSpeed);
		FNetControlMessage<NMT_Join>::Send(Connection);
		Connection->FlushNet(true);
		FinishRehome(true, Map);
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
		Twin->Destroy();
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
	return true;
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
	Conn->InitRemoteConnection(this, nullptr, FURL(), *Addr, USOCK_Open);
	AddClientConnection(Conn);
	if (Conn->Handler.IsValid()) Conn->Handler->BeginHandshaking();
	ByConn.Add(ConnId, Conn);
	UE_LOG(LogMusubiNet, Log, TEXT("relay %llu opened by first packet (entity %s)"), (unsigned long long)ConnId, *Conn->Entity);
	return Conn;
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
	if (bRehoming && FPlatformTime::Seconds() - RehomeStarted > RehomeTimeout) FinishRehome(false, TEXT("timeout"));
	else if (!bRehoming && TornOff.Num() && FPlatformTime::Seconds() - RehomeStarted > 2.0 * RehomeTimeout)
	{
		// entities the new owner never sent again (left our interest during the move): drop the stale twins
		for (TPair<FString, TWeakObjectPtr<AActor>>& KV : TornOff) if (AActor* A = KV.Value.Get()) A->Destroy();
		TornOff.Reset();
	}
	Super::TickDispatch(DeltaTime);
	DrainInbox();
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
	if (UMusubiSubsystem* Sub = Client.Get())
		if (TSharedPtr<FMusubiSocket> Sock = Sub->Socket())
		{
			Sock->OnTag(EMusubiServerMessageTag::NetPacket).Remove(PacketHandle);
			Sock->OnTag(EMusubiServerMessageTag::NetClose).Remove(CloseHandle);
			Sock->OnTag(EMusubiServerMessageTag::NetRehome).Remove(RehomeHandle);
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
