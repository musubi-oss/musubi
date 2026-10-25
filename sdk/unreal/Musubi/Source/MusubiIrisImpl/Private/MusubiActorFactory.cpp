#include "MusubiActorFactory.h"
#include "MusubiIrisModule.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiEntityRegistry.h"
#include "Engine/World.h"
#include "Engine/NetDriver.h"
#include "GameFramework/Actor.h"
#include "Iris/Serialization/NetSerializationContext.h"
#include "Iris/Serialization/NetBitStreamWriter.h"
#include "Iris/Serialization/NetBitStreamReader.h"
#include "Iris/ReplicationSystem/NetRefHandle.h"
#include "Net/Iris/ReplicationSystem/EngineReplicationBridge.h"
#include "MusubiIrisRehome.h"

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
using namespace UE::Net;

bool FMusubiActorNetCreationHeader::Serialize(const FCreationHeaderContext& Context) const
{
	if (!FDynamicActorNetCreationHeader::Serialize(Context)) return false;
	FNetBitStreamWriter* W = Context.Serialization.GetBitStreamWriter();
	if (W->WriteBool(!Entity.IsNil()))
		for (int32 I = 0; I < 16; I += 4)
			W->WriteBits(uint32(Entity.Bytes[I]) | uint32(Entity.Bytes[I + 1]) << 8 | uint32(Entity.Bytes[I + 2]) << 16 | uint32(Entity.Bytes[I + 3]) << 24, 32u);
	return !W->IsOverflown();
}

bool FMusubiActorNetCreationHeader::Deserialize(const FCreationHeaderContext& Context)
{
	if (!FDynamicActorNetCreationHeader::Deserialize(Context)) return false;
	FNetBitStreamReader* R = Context.Serialization.GetBitStreamReader();
	Entity = FMusubiUuid();
	if (R->ReadBool())
		for (int32 I = 0; I < 16; I += 4)
	{
		const uint32 V = R->ReadBits(32u);
		Entity.Bytes[I] = uint8(V); Entity.Bytes[I + 1] = uint8(V >> 8); Entity.Bytes[I + 2] = uint8(V >> 16); Entity.Bytes[I + 3] = uint8(V >> 24);
	}
	return !R->IsOverflown();
}

FString FMusubiActorNetCreationHeader::ToString() const
{
	return FString::Printf(TEXT("%s entity=%s"), *FDynamicActorNetCreationHeader::ToString(), *Entity.ToString());
}

namespace
{
	UMusubiAuthorityComponent* AuthorityOf(UObject* Obj)
	{
		AActor* A = Cast<AActor>(Obj);
		return A ? A->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
	}
}

TUniquePtr<FNetObjectCreationHeader> UMusubiActorFactory::CreateAndFillHeader(FNetRefHandle Handle)
{
	AActor* Actor = Cast<AActor>(Bridge->GetReplicatedObject(Handle));
	if (!Actor) return Super::CreateAndFillHeader(Handle);
	const FNetObjectReference Ref = Bridge->GetOrCreateObjectReference(Actor);
	// placed (stable name) and pre-registered actors: the engine's header, our bit says "not ours"
	if (!Ref.GetRefHandle().IsDynamic() || Bridge->IsNetRefHandlePreRegistered(Handle)) return Super::CreateAndFillHeader(Handle);
	TUniquePtr<FMusubiActorNetCreationHeader> H = MakeUnique<FMusubiActorNetCreationHeader>();
	if (!FillHeader(H.Get(), Handle, Actor)) return nullptr;
	if (const UMusubiAuthorityComponent* Auth = AuthorityOf(Actor)) H->Entity = Auth->GetEntityUuid();
	UE_LOG(LogMusubiIris, Verbose, TEXT("header for %s: %s"), *Actor->GetName(), *H->ToString());
	return H;
}

bool UMusubiActorFactory::SerializeHeader(const FCreationHeaderContext& Context, const FNetObjectCreationHeader* Header)
{
	const FBaseActorNetCreationHeader* Base = static_cast<const FBaseActorNetCreationHeader*>(Header);
	FNetBitStreamWriter* W = Context.Serialization.GetBitStreamWriter();
	const bool bOurs = Base->IsDynamic() && !Base->IsPreregistered(); // always an FMusubiActorNetCreationHeader, see CreateAndFillHeader
	if (!W->WriteBool(bOurs)) return Super::SerializeHeader(Context, Header);
	return Base->Serialize(Context);
}

TUniquePtr<FNetObjectCreationHeader> UMusubiActorFactory::CreateAndDeserializeHeader(const FCreationHeaderContext& Context)
{
	FNetBitStreamReader* R = Context.Serialization.GetBitStreamReader();
	if (!R->ReadBool()) return Super::CreateAndDeserializeHeader(Context);
	TUniquePtr<FMusubiActorNetCreationHeader> H = MakeUnique<FMusubiActorNetCreationHeader>();
	H->Deserialize(Context);
	return H;
}

UNetObjectFactory::FInstantiateResult UMusubiActorFactory::InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const FNetObjectCreationHeader* Header)
{
	const FBaseActorNetCreationHeader* Base = static_cast<const FBaseActorNetCreationHeader*>(Header);
	if (Base->IsDynamic() && !Base->IsPreregistered())
	{
		const FMusubiActorNetCreationHeader* H = static_cast<const FMusubiActorNetCreationHeader*>(Header);
		UEngineReplicationBridge* ActorBridge = CastChecked<UEngineReplicationBridge>(Bridge);
		UNetDriver* NetDriver = ActorBridge->GetNetDriver();
		UWorld* World = NetDriver ? NetDriver->GetWorld() : nullptr;
		UMusubiEntityRegistry* Reg = World ? UMusubiEntityRegistry::Get(World) : nullptr;
		const UObject* Archetype = Reg ? Bridge->ResolveObjectReference(H->ArchetypeReference, Context.ResolveContext) : nullptr;
		AActor* Existing = nullptr;
		if (Reg && !H->Entity.IsNil()) Existing = Reg->Resolve(H->Entity.ToString());
		else if (Archetype) Existing = Reg->RehomeKeptPlayerActor(Archetype->GetClass()); // the player's controller/state, rehome only
		if (IsValid(Existing))
		{
			if (Archetype && Existing->IsA(Archetype->GetClass()))
			{
				if (H->Entity.IsNil()) Reg->NoteRehomeKeptPlayerReused(Existing); else Reg->NoteIrisReused();
				UE_LOG(LogMusubiIris, Log, TEXT("rehome: reused %s in place (entity %s)"), *Existing->GetName(), *H->Entity.ToString());
				FInstantiateResult Result;
				Result.Instance = Existing;
				if (NetDriver->ShouldClientDestroyActor(Existing)) Result.Flags |= EReplicationBridgeCreateNetRefHandleResultFlags::AllowDestroyInstanceFromRemote;
				return Result;
			}
			UE_LOG(LogMusubiIris, Warning, TEXT("entity %s: existing %s is not a %s, spawning the new owner's actor"), *H->Entity.ToString(), *Existing->GetName(), Archetype ? *Archetype->GetClass()->GetName() : TEXT("?"));
		}
	}
	return Super::InstantiateReplicatedObjectFromHeader(Context, Header);
}

#if ENGINE_MINOR_VERSION >= 8
void UMusubiActorFactory::DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext)
{
	if (!SubObjectContext.IsSet() && Context.Reason != EDetachReason::Stopped && MusubiKeepActorOnDetach(Context.DetachedInstance)) return;
	Super::DetachedFromReplication(Context, SubObjectContext);
}
#else
void UMusubiActorFactory::DetachedFromReplication(const FDestroyedContext& Context)
{
	if (Context.DestroyReason != EReplicationBridgeDestroyInstanceReason::DoNotDestroy && MusubiKeepActorOnDetach(Context.DestroyedInstance)) return;
	Super::DetachedFromReplication(Context);
}
#endif
#endif // 5.7+
