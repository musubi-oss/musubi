#include "MusubiReplicationBridge.h"
#include "MusubiIrisModule.h"

#include "MusubiAuthorityComponent.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiJson.h"
#include "MusubiMatch.h" // FMusubiFnv1a64
#include "Engine/World.h"
#include "Engine/GameInstance.h"
#include "GameFramework/Actor.h"
#include "JsonObjectConverter.h"
#include "Iris/ReplicationSystem/ReplicationSystem.h"

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

	// Replicated UPROPERTYs of the object (and, for actors, of their replicated components) as one JSON
	// object: {"Prop": value, "CompName.Prop": value}. Transient/native-only data is skipped like Iris does.
	TSharedPtr<FJsonObject> ReplicatedToJson(UObject* Obj)
	{
		TSharedPtr<FJsonObject> Out = MakeShared<FJsonObject>();
		auto Walk = [&Out](UObject* O, const FString& Prefix)
		{
			for (TFieldIterator<FProperty> It(O->GetClass()); It; ++It)
			{
				FProperty* P = *It;
				if (!P->HasAnyPropertyFlags(CPF_Net) || P->HasAnyPropertyFlags(CPF_Transient | CPF_RepSkip))
					continue;
				TSharedPtr<FJsonValue> V = FJsonObjectConverter::UPropertyToJsonValue(P, P->ContainerPtrToValuePtr<void>(O));
				if (V.IsValid())
					Out->SetField(Prefix + P->GetName(), V);
			}
		};
		Walk(Obj, FString());
		if (AActor* A = Cast<AActor>(Obj))
			for (UActorComponent* C : A->GetReplicatedComponents())
				if (C) Walk(C, C->GetName() + TEXT("."));
		return Out;
	}

	uint64 HashJson(const TSharedPtr<FJsonObject>& O)
	{
		FMusubiFnv1a64 H;
		H.FeedUtf8(MusubiJson::Serialize(MakeShared<FJsonValueObject>(O)));
		return H.Get();
	}
}

UMusubiReplicationBridge::UMusubiReplicationBridge() {}

UMusubiSimulatorSubsystem* UMusubiReplicationBridge::Simulator() const
{
	UWorld* W = GetWorld();
	return (W && W->GetGameInstance()) ? W->GetGameInstance()->GetSubsystem<UMusubiSimulatorSubsystem>() : nullptr;
}

void UMusubiReplicationBridge::Initialize(UReplicationSystem* InReplicationSystem)
{
	Super::Initialize(InReplicationSystem);
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	// 5.8: DetachInstance is no longer virtual; detach is observed through the bridge delegates.
	RootDetachedHandle = GetRootObjectDetachedDelegate(this).AddLambda([this](FNetRefHandle H, UObject* O, EDetachReason) { Untrack(H, O); });
	SubObjectDetachedHandle = GetSubObjectDetachedDelegate(this).AddLambda([this](FNetRefHandle H, UObject* O, FNetRefHandle, UObject*, EDetachReason) { Untrack(H, O); });
#endif
}

void UMusubiReplicationBridge::Deinitialize()
{
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	GetRootObjectDetachedDelegate(this).Remove(RootDetachedHandle);
	GetSubObjectDetachedDelegate(this).Remove(SubObjectDetachedHandle);
#endif
	Tracked.Empty();
	Super::Deinitialize();
}

bool UMusubiReplicationBridge::CacheNetRefHandleCreationInfo(FNetRefHandle Handle)
{
	const bool bOk = Super::CacheNetRefHandleCreationInfo(Handle);
	// Called once when an object starts replicating (before its creation header is built): that is our begin hook.
	if (bOk)
		Track(Handle, GetReplicatedObject(Handle));
	return bOk;
}

void UMusubiReplicationBridge::Track(FNetRefHandle Handle, UObject* Object)
{
	UMusubiAuthorityComponent* Auth = AuthorityOf(Object);
	if (!Object || !Auth)
		return; // not a Musubi entity: Iris replicates it as usual, nothing to mirror
	FTracked& T = Tracked.FindOrAdd(Handle);
	T.Object = Object;
	T.Authority = Auth;
	T.LastHash = 0; // force a first full mirror
	DirtyThisUpdate.Add(Handle);
}

void UMusubiReplicationBridge::Untrack(FNetRefHandle Handle, UObject* Object)
{
	FTracked* T = Tracked.Find(Handle);
	if (!T)
		return;
	if (UMusubiAuthorityComponent* Auth = T->Authority.Get())
		if (Auth->IsOwned())
			Auth->RequestDespawn(); // detach from Iris == despawn in Musubi (torn-off/destroyed)
	Tracked.Remove(Handle);
	DirtyThisUpdate.Remove(Handle);
}

#if !(ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8)
void UMusubiReplicationBridge::DetachInstance(FNetRefHandle Handle)
{
	Untrack(Handle, GetReplicatedObject(Handle));
	Super::DetachInstance(Handle);
}
#endif

void UMusubiReplicationBridge::OnStartPreSendUpdate()
{
	Super::OnStartPreSendUpdate();
	bInSendUpdate = true;
}

void UMusubiReplicationBridge::PreSendUpdate()
{
	Super::PreSendUpdate();
	FlushDirty();
	bInSendUpdate = false;
}

void UMusubiReplicationBridge::FlushDirty()
{
	// Iris has just polled dirty state (PreSendUpdate). We mirror owned Musubi entities whose replicated
	// property set changed since the last send into the authority component; the simulator subsystem
	// picks it up as `apply` at the next tick_done. Hash-gated so an unchanged object costs one serialize.
	for (auto It = Tracked.CreateIterator(); It; ++It)
	{
		FTracked& T = It.Value();
		UObject* Obj = T.Object.Get();
		UMusubiAuthorityComponent* Auth = T.Authority.Get();
		if (!Obj || !Auth) { It.RemoveCurrent(); continue; }
		if (!Auth->IsOwned()) continue; // ghosts are driven by the node, never mirrored back
		TSharedPtr<FJsonObject> J = ReplicatedToJson(Obj);
		const uint64 H = HashJson(J);
		if (H == T.LastHash) continue;
		T.LastHash = H;
		Auth->SetComponent(TEXT("iris"), FMusubiJson(J));
	}
	DirtyThisUpdate.Reset();
}

void UMusubiReplicationBridge::UpdateInstancesWorldLocation()
{
	Super::UpdateInstancesWorldLocation();
	// Musubi x/y (cm) come from the actor transform; the authority component already tracks the owner's
	// location each simulator tick, so there is nothing extra to push here — kept as an explicit no-op
	// so the Iris world-location pass and the Musubi cell position stay the same source of truth.
}

void UMusubiReplicationBridge::PruneStaleObjects()
{
	Super::PruneStaleObjects();
	for (auto It = Tracked.CreateIterator(); It; ++It)
		if (!It.Value().Object.IsValid()) It.RemoveCurrent();
}

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
void UMusubiReplicationBridge::OnProtocolMismatchReported(FNetRefHandle RefHandle, uint32 ConnectionId, const TArray<uint8>& ClientCDOStateBytes)
{
	Super::OnProtocolMismatchReported(RefHandle, ConnectionId, ClientCDOStateBytes);
#else
void UMusubiReplicationBridge::OnProtocolMismatchReported(FNetRefHandle RefHandle, uint32 ConnectionId)
{
	Super::OnProtocolMismatchReported(RefHandle, ConnectionId);
#endif
	if (const FTracked* T = Tracked.Find(RefHandle))
		if (UMusubiAuthorityComponent* Auth = T->Authority.Get())
			UE_LOG(LogMusubiIris, Warning, TEXT("protocol mismatch on entity %s (conn %u): client build does not match server replicated layout"), *Auth->GetEntityId(), ConnectionId);
}
