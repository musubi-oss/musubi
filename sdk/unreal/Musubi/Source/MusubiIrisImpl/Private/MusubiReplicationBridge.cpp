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
#include "Iris/ReplicationState/ReplicationStateDescriptorBuilder.h"
#include "Iris/ReplicationSystem/ReplicationOperations.h"
#include "Iris/Serialization/NetBitStreamWriter.h"
#include "Iris/Serialization/NetSerializationContext.h"
#include "Misc/Base64.h"

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
}

void UMusubiReplicationBridge::Deinitialize()
{
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
	FreeStates(*T, T->Object.Get());
	Tracked.Remove(Handle);
	DirtyThisUpdate.Remove(Handle);
}

const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& UMusubiReplicationBridge::DescriptorsFor(UClass* Class)
{
	if (TArray<TRefCountPtr<const FReplicationStateDescriptor>>* Found = ClassDescriptors.Find(Class)) return *Found;
	TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Out = ClassDescriptors.Add(Class);
	FReplicationStateDescriptorBuilder::FParameters P;
	P.DefaultStateSource = Class->GetDefaultObject();
	FReplicationStateDescriptorBuilder::CreateDescriptorsForClass(Out, Class, P);
	return Out;
}

void UMusubiReplicationBridge::FreeStates(FTracked& T, UObject* Obj)
{
	if (!Obj || T.States.Num() == 0) return;
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	FNetSerializationContext Ctx;
	for (int32 i = 0; i < T.States.Num() && i < Descs.Num(); ++i)
	{
		FTracked::FStateBuf& S = T.States[i];
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D || S.Internal.Num() == 0) continue;
		FReplicationStateOperations::FreeDynamicState(Ctx, S.Internal.GetData(), D);
		if (S.bHasLast) FReplicationStateOperations::FreeDynamicState(Ctx, S.Last.GetData(), D);
	}
	T.States.Reset();
}

// The `iris` component, binary form: {"c": class path, "s": [{"i": descriptor index, "bits": n, "b": base64 of
// FReplicationStateOperations::Serialize of the quantized state}]}. Only descriptors without object references
// are exported this way — resolving references needs Iris' internal serialization context, which is engine-private;
// classes whose every descriptor has references fall back to the JSON mirror. Unchanged states (IsEqualQuantizedState
// against the last export) are not re-sent; a message carries only the descriptors that changed.
bool UMusubiReplicationBridge::ExportBinary(FTracked& T, UObject* Obj, TSharedPtr<FJsonObject>& Out, bool& bChanged)
{
	bChanged = false;
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	if (Descs.Num() == 0) return false;
	if (T.States.Num() != Descs.Num()) T.States.SetNum(Descs.Num());
	bool bEligible = false;
	TArray<TSharedPtr<FJsonValue>> States;
	for (int32 i = 0; i < Descs.Num(); ++i)
	{
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D || D->ObjectReferenceCount > 0 || D->MemberCount == 0) continue;
		bEligible = true;
		FTracked::FStateBuf& S = T.States[i];
		if (S.External.Num() != (int32)D->ExternalSize)
		{
			S.External.SetNumZeroed(D->ExternalSize);
			S.Internal.SetNumZeroed(D->InternalSize);
			S.Last.SetNumZeroed(D->InternalSize);
			S.bHasLast = false;
		}
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			const FProperty* P = D->MemberProperties ? D->MemberProperties[m] : nullptr;
			if (!P) continue;
			const uint16 ArrayIndex = D->MemberPropertyDescriptors ? D->MemberPropertyDescriptors[m].ArrayIndex : 0;
			P->CopySingleValue(S.External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset, P->ContainerPtrToValuePtr<void>(Obj, ArrayIndex));
		}
		FNetSerializationContext Ctx;
		FReplicationStateOperations::Quantize(Ctx, S.Internal.GetData(), S.External.GetData(), D);
		if (S.bHasLast && FReplicationStateOperations::IsEqualQuantizedState(Ctx, S.Internal.GetData(), S.Last.GetData(), D)) continue;
		if (S.bHasLast) FReplicationStateOperations::FreeDynamicState(Ctx, S.Last.GetData(), D);
		FMemory::Memcpy(S.Last.GetData(), S.Internal.GetData(), D->InternalSize);
		FMemory::Memzero(S.Internal.GetData(), D->InternalSize); // dynamic state now owned by Last
		S.bHasLast = true;
		uint8 Buf[8192];
		FNetBitStreamWriter W;
		W.InitBytes(Buf, sizeof(Buf));
		FNetSerializationContext WCtx(&W);
		FReplicationStateOperations::Serialize(WCtx, S.Last.GetData(), D);
		W.CommitWrites();
		if (W.IsOverflown())
		{
			UE_LOG(LogMusubiIris, Warning, TEXT("%s descriptor %d: state over %d bytes, not exported"), *Obj->GetName(), i, (int32)sizeof(Buf));
			continue;
		}
		const uint32 Bits = W.GetPosBits();
		TSharedPtr<FJsonObject> St = MakeShared<FJsonObject>();
		St->SetNumberField(TEXT("i"), i);
		St->SetNumberField(TEXT("bits"), Bits);
		St->SetStringField(TEXT("b"), FBase64::Encode(Buf, (Bits + 7) / 8));
		States.Add(MakeShared<FJsonValueObject>(St));
		bChanged = true;
	}
	if (!bEligible) return false;
	if (bChanged)
	{
		Out = MakeShared<FJsonObject>();
		Out->SetStringField(TEXT("c"), Obj->GetClass()->GetPathName());
		Out->SetArrayField(TEXT("s"), States);
	}
	return true;
}

#if !(ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8)
void UMusubiReplicationBridge::DetachInstance(FNetRefHandle Handle)
{
	Untrack(Handle, GetReplicatedObject(Handle));
	Super::DetachInstance(Handle);
}
#else
// 5.8: no virtual detach hook and the bridge delegates are private — see the header.
void UMusubiReplicationBridge::SweepDetached()
{
	for (auto It = Tracked.CreateIterator(); It; ++It)
		if (GetReplicatedObject(It.Key()) == nullptr)
		{
			FTracked T = It.Value();
			It.RemoveCurrent();
			if (UMusubiAuthorityComponent* Auth = T.Authority.Get())
				if (Auth->IsOwned())
					Auth->RequestDespawn();
		}
}
#endif

void UMusubiReplicationBridge::OnStartPreSendUpdate()
{
	Super::OnStartPreSendUpdate();
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	SweepDetached();
#endif
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
		TSharedPtr<FJsonObject> Bin;
		bool bChanged = false;
		if (ExportBinary(T, Obj, Bin, bChanged))
		{
			if (bChanged) Auth->SetComponent(TEXT("iris"), FMusubiJson(Bin));
			continue;
		}
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
