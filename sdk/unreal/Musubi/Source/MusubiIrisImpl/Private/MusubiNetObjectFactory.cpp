#include "MusubiNetObjectFactory.h"
#include "MusubiIrisModule.h"
#include "MusubiAuthorityComponent.h"
#include "GameFramework/Actor.h"
#include "Engine/World.h"
#include "UObject/UObjectGlobals.h"
#include "Engine/GameInstance.h"
#include "MusubiSimulatorSubsystem.h"

const FName UMusubiNetObjectFactory::FactoryName(TEXT("MusubiEntity"));

#include "Iris/Serialization/NetBitStreamUtil.h"
#include "Iris/Serialization/NetBitStreamWriter.h"
#include "Iris/Serialization/NetBitStreamReader.h"
#include "Iris/Serialization/NetSerializationContext.h"
#include "Iris/ReplicationSystem/ObjectReplicationBridge.h"

using namespace UE::Net;

namespace
{
	UMusubiAuthorityComponent* AuthorityOf(UObject* Obj)
	{
		if (AActor* A = Cast<AActor>(Obj))
			return A->FindComponentByClass<UMusubiAuthorityComponent>();
		if (UActorComponent* C = Cast<UActorComponent>(Obj))
			if (AActor* Owner = C->GetOwner())
				return Owner->FindComponentByClass<UMusubiAuthorityComponent>();
		return nullptr;
	}
}

TUniquePtr<FNetObjectCreationHeader> UMusubiNetObjectFactory::CreateAndFillHeader(FNetRefHandle Handle)
{
	TUniquePtr<FMusubiCreationHeader> H = MakeUnique<FMusubiCreationHeader>();
	if (Bridge) // protected member set by UNetObjectFactory::Init (5.7 NetObjectFactory.h L314)
	{
		if (UObject* Obj = Bridge->GetReplicatedObject(Handle))
		{
			H->ClassPath = Obj->GetClass()->GetPathName();
			H->bIsActor = Obj->IsA<AActor>();
			if (UMusubiAuthorityComponent* Auth = AuthorityOf(Obj))
				H->Entity = Auth->GetEntityUuid();
		}
	}
	return H;
}

bool UMusubiNetObjectFactory::SerializeHeader(const FCreationHeaderContext& Context, const FNetObjectCreationHeader* Header)
{
	const FMusubiCreationHeader* H = static_cast<const FMusubiCreationHeader*>(Header);
	FNetBitStreamWriter* W = Context.Serialization.GetBitStreamWriter();
	WriteString(W, H->ClassPath);
	W->WriteBits(H->bIsActor ? 1u : 0u, 1u);
	WriteBytes(W, H->Entity.Bytes, 16);
	return !W->IsOverflown();
}

TUniquePtr<FNetObjectCreationHeader> UMusubiNetObjectFactory::CreateAndDeserializeHeader(const FCreationHeaderContext& Context)
{
	TUniquePtr<FMusubiCreationHeader> H = MakeUnique<FMusubiCreationHeader>();
	FNetBitStreamReader* R = Context.Serialization.GetBitStreamReader();
	ReadString(R, H->ClassPath);
	H->bIsActor = R->ReadBits(1u) != 0u;
	ReadBytes(R, H->Entity.Bytes, 16);
	return H;
}

UNetObjectFactory::FInstantiateResult UMusubiNetObjectFactory::InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const FNetObjectCreationHeader* Header)
{
	FInstantiateResult Result;
	const FMusubiCreationHeader* H = static_cast<const FMusubiCreationHeader*>(Header);
	UClass* Cls = LoadClass<UObject>(nullptr, *H->ClassPath);
	if (!Cls)
	{
		UE_LOG(LogMusubiIris, Warning, TEXT("cannot resolve class %s for %s"), *H->ClassPath, *Context.Handle.ToString());
		return Result;
	}
	UWorld* World = Bridge ? Bridge->GetWorld() : nullptr; // UObject outer chain: bridge → replication system → net driver → world
	if (H->bIsActor && World)
	{
		FActorSpawnParameters P;
		P.SpawnCollisionHandlingOverride = ESpawnActorCollisionHandlingMethod::AlwaysSpawn;
		P.bNoFail = true;
		P.bDeferConstruction = true; // like UEngineReplicationBridge: BeginPlay after state is applied
		Result.Instance = World->SpawnActor(Cls, nullptr, nullptr, P);
	}
	else
	{
		Result.Instance = NewObject<UObject>(World ? (UObject*)World : GetTransientPackage(), Cls);
	}
	return Result;
}

void UMusubiNetObjectFactory::PostInstantiation(const FPostInstantiationContext& Context)
{
	const FMusubiCreationHeader* H = static_cast<const FMusubiCreationHeader*>(Context.Header);
	if (!Context.Instance || !H) return;
	if (UMusubiAuthorityComponent* Auth = AuthorityOf(Context.Instance))
	{
		UWorld* World = Context.Instance->GetWorld();
		UMusubiSimulatorSubsystem* Sim = (World && World->GetGameInstance()) ? World->GetGameInstance()->GetSubsystem<UMusubiSimulatorSubsystem>() : nullptr;
		Auth->Bind(Sim, H->Entity, FMusubiCellId(), /*bIsOwned*/ false); // ghost until Musubi hands authority over
	}
	if (AActor* A = Cast<AActor>(Context.Instance))
		if (A->IsActorInitialized() == false)
			A->FinishSpawning(A->GetActorTransform(), true);
}

// Iris says the remote object is gone; the Musubi side mirrors despawn through the bridge's detach path.
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
void UMusubiNetObjectFactory::DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext)
{
	if (SubObjectContext.IsSet()) return; // sub-objects die with their root
	if (AActor* A = Cast<AActor>(Context.DetachedInstance))
		if (Context.Reason != EDetachReason::Stopped) // Stopped = still alive locally, only no longer replicated
			A->Destroy();
}
#else
void UMusubiNetObjectFactory::DetachedFromReplication(const FDestroyedContext& Context)
{
	if (AActor* A = Cast<AActor>(Context.DestroyedInstance))
		if (Context.DestroyReason != EReplicationBridgeDestroyInstanceReason::DoNotDestroy)
			A->Destroy();
}
#endif
