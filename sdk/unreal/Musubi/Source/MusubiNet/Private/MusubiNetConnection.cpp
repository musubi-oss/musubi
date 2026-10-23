#include "MusubiNetConnection.h"
#include "PacketHandler.h"

#include "MusubiNetDriver.h"
#include "Net/DataChannel.h"

UMusubiNetConnection::UMusubiNetConnection(const FObjectInitializer& ObjectInitializer)
	: Super(ObjectInitializer)
{
}

/** Per-packet cost of the relay envelope (`net_packet` + conn + base64 padding), the engine asserts PacketOverhead > 0. */
static constexpr int32 RelayPacketOverhead = 48;

void UMusubiNetConnection::InitBase(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	const UMusubiNetDriver* D = Cast<UMusubiNetDriver>(InDriver);
	// no UDP/IP headers underneath: the relay frame is the packet
	Super::InitBase(InDriver, InSocket, InURL, InState, FMath::Min(InMaxPacket == 0 ? (D ? D->MaxRelayPacket : MAX_PACKET_SIZE) : InMaxPacket, (int32)MAX_PACKET_SIZE), InPacketOverhead > 0 ? InPacketOverhead : RelayPacketOverhead);
}

void UMusubiNetConnection::InitLocalConnection(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	InitBase(InDriver, InSocket, InURL, InState, InMaxPacket, InPacketOverhead);
	World = InURL.Host;
	Entity = InURL.GetOption(TEXT("entity="), TEXT(""));
	InitSendBuffer();
}

void UMusubiNetConnection::InitRemoteConnection(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, const FInternetAddr& InRemoteAddr, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	InitBase(InDriver, InSocket, InURL, InState, InMaxPacket, InPacketOverhead);
	RemoteAddr = InRemoteAddr.Clone();
	InitSendBuffer();
	// a client that still has to log in (NMT_Hello first), exactly as UIpConnection
	SetClientLoginState(EClientLoginState::LoggingIn);
	SetExpectedClientLoginMsgType(NMT_Hello);
}

void UMusubiNetConnection::Opened(uint64 InConnId, int32 InMaxPacket)
{
	ConnId = InConnId;
	if (InMaxPacket > 0 && InMaxPacket < MaxPacket) MaxPacket = InMaxPacket;
	UMusubiNetDriver* D = Cast<UMusubiNetDriver>(Driver);
	TArray<TArray<uint8>> Out = MoveTemp(PendingOut);
	PendingOut.Reset();
	if (D)
		for (const TArray<uint8>& P : Out) D->SendPacket(this, P.GetData(), P.Num());
}

void UMusubiNetConnection::InitHandler()
{
	// No PacketHandler at all (the engine's -NoPacketHandler path, here in every configuration): with the stateless-connect
	// component the cookie exchange needs connectionless sends a relay has none of, and without any component
	// PacketHandler::BeginHandshaking never fires its completion delegate, so UPendingNetGame would never send NMT_Hello.
	// The relay is already authenticated by the node and encrypted/compressed by the transport, so nothing is lost.
	check(!Handler.IsValid());
	MaxPacketHandlerBits = 0;
}

void UMusubiNetConnection::LowLevelSend(void* Data, int32 CountBits, FOutPacketTraits& Traits)
{
	const uint8* ToSend = reinterpret_cast<const uint8*>(Data);
	if (Handler.IsValid() && !Handler->GetRawSend())
	{
		const ProcessedPacket P = Handler->Outgoing(reinterpret_cast<uint8*>(Data), CountBits, Traits);
		if (P.bError) return;
		ToSend = P.Data;
		CountBits = P.CountBits;
	}
	const int32 Count = FMath::DivideAndRoundUp(CountBits, 8);
	if (Count <= 0) return;
	if (Count > MaxPacket)
		UE_LOG(LogNet, Warning, TEXT("UMusubiNetConnection::LowLevelSend: %d bytes > MaxPacket %d (%s)"), Count, MaxPacket, *LowLevelDescribe());
	bool bBlock = false;
#if !UE_BUILD_SHIPPING
	LowLevelSendDel.ExecuteIfBound((void*)ToSend, Count, bBlock);
#endif
	if (bBlock) return;
	if (!IsOpen())
	{
		// client before `net_opened`: the engine's hello goes out as soon as the relay exists
		if (PendingOut.Num() < 64) PendingOut.Emplace(ToSend, Count);
		return;
	}
	if (UMusubiNetDriver* D = Cast<UMusubiNetDriver>(Driver)) D->SendPacket(this, ToSend, Count);
}

FString UMusubiNetConnection::LowLevelGetRemoteAddress(bool bAppendPort)
{
	return FString::Printf(TEXT("musubi:%llu"), (unsigned long long)ConnId);
}

FString UMusubiNetConnection::LowLevelDescribe()
{
	return FString::Printf(TEXT("relay=%llu world=%s entity=%s state: %s"), (unsigned long long)ConnId, *World, *Entity, LexToString(GetConnectionState()));
}

void UMusubiNetConnection::CleanUp()
{
	PendingOut.Reset();
	Super::CleanUp();
}
