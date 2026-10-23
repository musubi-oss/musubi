#include "MusubiReplicationBridge.h"
#include "Iris/Serialization/NetSerializer.h"
#include "Iris/ReplicationState/PropertyNetSerializerInfoRegistry.h"
#include "Engine/EngineTypes.h"
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
#include "Iris/Serialization/NetBitStreamReader.h"
#include "Iris/Serialization/NetSerializationContext.h"
#include "Misc/Base64.h"
#include "Net/Core/NetBitArray.h"
#include "MusubiSimulatorSubsystem.h"
#include "Components/ActorComponent.h"

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
	if (!T.Object.IsValid()) UE_LOG(LogMusubiIris, Log, TEXT("mirroring %s (entity %s)"), *Object->GetName(), *Auth->GetEntityId());
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

// Members whose serializer carries object references are left out of the Iris mask (resolving them needs the
// engine-private FInternalNetSerializationContext) and travel beside the bit stream as Musubi ids:
// "<entity uuid>" for an actor with an authority component, "<uuid>/<component name>" for one of its components.
static bool IsRefMember(const FReplicationStateDescriptor* D, uint32 m)
{
	return D->MemberTraitsDescriptors && EnumHasAnyFlags(D->MemberTraitsDescriptors[m].Traits, EReplicationStateMemberTraits::HasObjectReference);
}

// Members whose serializer is connection-specific (FNetRoleNetSerializer: Role/RemoteRole swap per connection) need
// a real connection in the context; we serialize with none, and ghosts set their roles locally anyway.
static bool IsConnectionSpecificMember(const FReplicationStateDescriptor* D, uint32 m)
{
	// The builder folds this trait into the state descriptor, not the member one, so ask the serializer itself.
	if (D->MemberSerializerDescriptors && D->MemberSerializerDescriptors[m].Serializer
		&& EnumHasAnyFlags(D->MemberSerializerDescriptors[m].Serializer->Traits, ENetSerializerTraits::HasConnectionSpecificSerialization))
		return true;
	// FNetRoleNetSerializer (Role/RemoteRole) does not declare the trait yet dereferences the internal context
	// unconditionally; the engine picks it for byte properties whose enum is ENetRole, so match the same way.
	if (D->MemberProperties)
		if (const FByteProperty* BP = CastField<FByteProperty>(D->MemberProperties[m]))
			if (BP->Enum && BP->Enum == StaticEnum<ENetRole>()) return true;
	return D->MemberTraitsDescriptors && EnumHasAnyFlags(D->MemberTraitsDescriptors[m].Traits, EReplicationStateMemberTraits::HasConnectionSpecificSerialization);
}
// Iris adds AActor::NetCullDistanceSquared to the actor's state with a nop serializer purely so its own filtering can
// read it: it is never in the bitstream, so it must neither be masked in nor written back.
static bool IsNopMember(const FReplicationStateDescriptor* D, uint32 m)
{
	if (!D->MemberSerializerDescriptors || !D->MemberProperties) return false;
	const FPropertyNetSerializerInfo* Nop = FPropertyNetSerializerInfoRegistry::GetNopNetSerializerInfo();
	return Nop && D->MemberSerializerDescriptors[m].Serializer == Nop->GetNetSerializer(D->MemberProperties[m]);
}

static FNetBitArrayView ValueMask(const FReplicationStateDescriptor* D, TArray<uint32>& Storage)
{
	Storage.SetNumZeroed(FMath::Max(1, (int32)((D->ChangeMaskBitCount + 31) / 32)));
	FNetBitArrayView V(Storage.GetData(), D->ChangeMaskBitCount, FNetBitArrayView::ResetOnInit);
	for (uint32 m = 0; m < D->MemberCount; ++m)
	{
		if (IsRefMember(D, m) || IsConnectionSpecificMember(D, m) || IsNopMember(D, m) || !D->MemberChangeMaskDescriptors) continue;
		const FReplicationStateMemberChangeMaskDescriptor& C = D->MemberChangeMaskDescriptors[m];
		for (uint16 b = 0; b < C.BitCount; ++b) V.SetBit(C.BitOffset + b);
	}
	return V;
}

static FString RefId(UObject* O)
{
	if (!O) return FString();
	if (const AActor* A = Cast<AActor>(O))
		if (const UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>()) return Auth->GetEntityId();
	if (const UActorComponent* C = Cast<UActorComponent>(O))
		if (const AActor* A = C->GetOwner())
			if (const UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>()) return Auth->GetEntityId() + TEXT("/") + C->GetName();
	return FString();
}

static UObject* ResolveRef(UMusubiSimulatorSubsystem* Sim, const FString& Id)
{
	if (!Sim || Id.IsEmpty()) return nullptr;
	FString Uuid, Comp;
	if (!Id.Split(TEXT("/"), &Uuid, &Comp)) Uuid = Id;
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(Uuid, U)) return nullptr;
	UMusubiAuthorityComponent* Auth = Sim->FindAuthority(U);
	AActor* A = Auth ? Auth->GetOwner() : nullptr;
	if (!A || Comp.IsEmpty()) return A;
	for (UActorComponent* C : A->GetComponents()) if (C && C->GetName() == Comp) return C;
	return nullptr;
}

const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& UMusubiReplicationBridge::DescriptorsFor(UClass* Class)
{
	if (TArray<TRefCountPtr<const FReplicationStateDescriptor>>* Found = ClassDescriptors.Find(Class)) return *Found;
	TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Out = ClassDescriptors.Add(Class);
	FReplicationStateDescriptorBuilder::FParameters P;
	// Same context the engine bridge hands to FFragmentRegistrationContext: without the registry the builder
	// dereferences a null registry (5.8 asserts in a scope lock on the first owned actor).
	P.DescriptorRegistry = GetReplicationStateDescriptorRegistry();
	P.ReplicationSystem = GetReplicationSystem();
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
		if (!D || D->MemberCount == 0) continue;
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
		TArray<uint32> MaskStorage;
		const FNetBitArrayView Mask = ValueMask(D, MaskStorage);
		TMap<FString, FString> Refs;
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			if (!IsRefMember(D, m) || !D->MemberProperties) continue;
			if (const FObjectPropertyBase* OP = CastField<FObjectPropertyBase>(D->MemberProperties[m]))
				Refs.Add(OP->GetName(), RefId(OP->GetObjectPropertyValue(S.External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset)));
		}
		const bool bRefsChanged = !S.bHasLast || !Refs.OrderIndependentCompareEqual(S.LastRefs);
		FNetSerializationContext Ctx;
		Ctx.SetChangeMask(&Mask); // the *WithMask operations check the context carries this exact mask
		FReplicationStateOperations::QuantizeWithMask(Ctx, Mask, 0, S.Internal.GetData(), S.External.GetData(), D);
		if (!bRefsChanged && S.bHasLast && FReplicationStateOperations::IsEqualQuantizedState(Ctx, S.Internal.GetData(), S.Last.GetData(), D)) continue;
		S.LastRefs = Refs;
		if (S.bHasLast) FReplicationStateOperations::FreeDynamicState(Ctx, S.Last.GetData(), D);
		FMemory::Memcpy(S.Last.GetData(), S.Internal.GetData(), D->InternalSize);
		FMemory::Memzero(S.Internal.GetData(), D->InternalSize); // dynamic state now owned by Last
		S.bHasLast = true;
		uint8 Buf[8192];
		FNetBitStreamWriter W;
		W.InitBytes(Buf, sizeof(Buf));
		FNetSerializationContext WCtx(&W);
		WCtx.SetChangeMask(&Mask);
		FReplicationStateOperations::SerializeWithMask(WCtx, Mask, 0, S.Last.GetData(), D);
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
		if (Refs.Num())
		{
			TSharedPtr<FJsonObject> R = MakeShared<FJsonObject>();
			for (const TPair<FString, FString>& KV : Refs) { if (KV.Value.IsEmpty()) R->SetField(KV.Key, MakeShared<FJsonValueNull>()); else R->SetStringField(KV.Key, KV.Value); }
			St->SetObjectField(TEXT("r"), R);
		}
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
			if (bChanged)
			{
				if (!T.bMirrored) UE_LOG(LogMusubiIris, Log, TEXT("%s: first binary iris state exported"), *Obj->GetName());
				T.bMirrored = true;
				Auth->SetComponent(TEXT("iris"), FMusubiJson(Bin));
			}
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

bool UMusubiReplicationBridge::ApplyBinary(UObject* Obj, const TSharedPtr<FJsonObject>& Iris, UMusubiSimulatorSubsystem* Sim)
{
	if (!Obj || !Iris.IsValid()) return false;
	const TArray<TSharedPtr<FJsonValue>>* States = nullptr;
	if (!Iris->TryGetArrayField(TEXT("s"), States) || !States) return false; // JSON mirror form, not ours
	FString ClassPath;
	if (Iris->TryGetStringField(TEXT("c"), ClassPath) && ClassPath != Obj->GetClass()->GetPathName())
	{
		UE_LOG(LogMusubiIris, Warning, TEXT("%s: iris state is for %s, ghost is %s — skipped"), *Obj->GetName(), *ClassPath, *Obj->GetClass()->GetPathName());
		return false;
	}
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	bool bApplied = false;
	for (const TSharedPtr<FJsonValue>& V : *States)
	{
		const TSharedPtr<FJsonObject>* St = nullptr;
		if (!V.IsValid() || !V->TryGetObject(St) || !St) continue;
		const int32 i = (int32)(*St)->GetNumberField(TEXT("i"));
		const uint32 Bits = (uint32)(*St)->GetNumberField(TEXT("bits"));
		TArray<uint8> Bytes;
		if (!Descs.IsValidIndex(i) || !FBase64::Decode((*St)->GetStringField(TEXT("b")), Bytes) || Bytes.Num() * 8 < (int32)Bits) continue;
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D) continue;
		TArray<uint32> MaskStorage;
		const FNetBitArrayView Mask = ValueMask(D, MaskStorage);
		const TSharedPtr<FJsonObject>* Refs = nullptr;
		(*St)->TryGetObjectField(TEXT("r"), Refs);
		TArray<uint8> Internal, External;
		Internal.SetNumZeroed(D->InternalSize);
		External.SetNumZeroed(D->ExternalSize);
		FNetBitStreamReader R;
		R.InitBits(Bytes.GetData(), Bits);
		FNetSerializationContext RCtx(&R);
		RCtx.SetChangeMask(&Mask);
		FReplicationStateOperations::DeserializeWithMask(RCtx, Mask, 0, Internal.GetData(), D);
		if (R.IsOverflown() || RCtx.HasError())
		{
			UE_LOG(LogMusubiIris, Warning, TEXT("%s descriptor %d: bad iris state (%u bits)"), *Obj->GetName(), i, Bits);
			FNetSerializationContext Ctx;
			FReplicationStateOperations::FreeDynamicState(Ctx, Internal.GetData(), D);
			continue;
		}
		FNetSerializationContext Ctx;
		Ctx.SetChangeMask(&Mask);
		FReplicationStateOperations::DequantizeWithMask(Ctx, Mask, 0, External.GetData(), Internal.GetData(), D);
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			const FProperty* P = D->MemberProperties ? D->MemberProperties[m] : nullptr;
			if (!P) continue;
			// Not in the bitstream (Role/RemoteRole, the nop NetCullDistanceSquared): External holds zeros here, and writing
			// ROLE_None or a zero cull distance onto the ghost would strip HasAuthority() / relevancy once it becomes the owner.
			if (IsConnectionSpecificMember(D, m) || IsNopMember(D, m)) continue;
			const uint16 ArrayIndex = D->MemberPropertyDescriptors ? D->MemberPropertyDescriptors[m].ArrayIndex : 0;
			void* Dst = P->ContainerPtrToValuePtr<void>(Obj, ArrayIndex);
			uint8* Src = External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset;
			bool bSame = true;
			if (IsRefMember(D, m))
			{
				const FObjectPropertyBase* OP = CastField<FObjectPropertyBase>(P);
				if (!OP || !Refs || !(*Refs)->HasField(P->GetName())) continue; // not carried: keep what the ghost has
				FString Id;
				(*Refs)->TryGetStringField(P->GetName(), Id);
				UObject* Want = ResolveRef(Sim, Id);
				if (!Id.IsEmpty() && !Want) continue; // the referenced entity is not on this server (yet)
				bSame = OP->GetObjectPropertyValue(Dst) == Want;
				OP->SetObjectPropertyValue(Dst, Want);
			}
			else
			{
				bSame = P->Identical(Dst, Src);
				P->CopySingleValue(Dst, Src);
				P->DestroyValue(Src);
			}
			if (!bSame && D->MemberPropertyDescriptors)
				if (const UFunction* Fn = D->MemberPropertyDescriptors[m].RepNotifyFunction)
					if (Fn->NumParms == 0) Obj->ProcessEvent(const_cast<UFunction*>(Fn), nullptr);
		}
		FReplicationStateOperations::FreeDynamicState(Ctx, Internal.GetData(), D);
		bApplied = true;
	}
	return bApplied;
}
