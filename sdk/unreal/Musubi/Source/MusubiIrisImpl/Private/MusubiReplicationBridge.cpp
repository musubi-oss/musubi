#include "MusubiReplicationBridge.h"
#include "Net/Serialization/FastArraySerializer.h"

static bool ApplyFastArrayFromIris(const FProperty* P, void* Dst, const void* Src, bool bSame);
#include "Serialization/MemoryWriter.h"
#include "Serialization/MemoryReader.h"
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
	FreeStates(T->States, T->Object.Get());
	if (AActor* A = Cast<AActor>(T->Object.Get()))
		for (TPair<FName, TArray<FTracked::FStateBuf>>& KV : T->Subs)
		{
			UObject* Sub = nullptr;
			for (UActorComponent* C : A->GetComponents()) if (C && C->GetFName() == KV.Key) { Sub = C; break; }
			if (!Sub) if (UMusubiAuthorityComponent* Auth = T->Authority.Get()) Sub = Auth->FindMusubiSubObject(KV.Key.ToString());
			if (Sub) FreeStates(KV.Value, Sub);
		}
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
	// a registered subobject of an entity (RegisterMusubiSubObject): same id form as a component, resolved by name on the
	// receiver's registry — the receiver created its own instance, the owner's object path means nothing there
	for (UObject* Outer = O->GetOuter(); Outer; Outer = Outer->GetOuter())
		if (const AActor* A = Cast<AActor>(Outer))
		{
			if (const UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>())
				if (Auth->MusubiSubObjects().Contains(O)) return Auth->GetEntityId() + TEXT("/") + O->GetName();
			break;
		}
	// not a Musubi entity: a level-placed actor (or a component of one) exists under the same name on every server
	// that has the level loaded, so its path is a stable id; dynamically spawned non-entity objects are not carried
	const AActor* Owner = Cast<AActor>(O);
	if (!Owner) if (const UActorComponent* C = Cast<UActorComponent>(O)) Owner = C->GetOwner();
	if (Owner && Owner->IsNetStartupActor() && O->IsNameStableForNetworking()) return TEXT("path:") + O->GetPathName();
	return FString();
}

static UObject* ResolveRef(UMusubiSimulatorSubsystem* Sim, const FString& Id)
{
	if (!Sim || Id.IsEmpty()) return nullptr;
	if (Id.StartsWith(TEXT("path:"))) return FindObject<UObject>(nullptr, *Id.Mid(5));
	FString Uuid, Comp;
	if (!Id.Split(TEXT("/"), &Uuid, &Comp)) Uuid = Id;
	FMusubiUuid U;
	if (!FMusubiUuid::Parse(Uuid, U)) return nullptr;
	UMusubiAuthorityComponent* Auth = Sim->FindAuthority(U);
	AActor* A = Auth ? Auth->GetOwner() : nullptr;
	if (!A || Comp.IsEmpty()) return A;
	for (UActorComponent* C : A->GetComponents()) if (C && C->GetName() == Comp) return C;
	return Auth->FindMusubiSubObject(Comp);
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

void UMusubiReplicationBridge::DisablePushDirtiness(UClass* Class)
{
	// The builder registers class descriptors in the registry it is given and FReplicationFragmentUtil reuses them
	// when the fragments are created, so clearing the trait here is what the fragments (and so
	// SetNetPushIdOnInstance) will see.
	for (const TRefCountPtr<const FReplicationStateDescriptor>& Ref : DescriptorsFor(Class))
	{
		if (FReplicationStateDescriptor* D = const_cast<FReplicationStateDescriptor*>(Ref.GetReference()))
		{
			EnumRemoveFlags(D->Traits, EReplicationStateTraits::HasPushBasedDirtiness);
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
			EnumRemoveFlags(D->Traits, EReplicationStateTraits::HasFullPushBasedDirtiness);
#endif
		}
	}
}

void UMusubiReplicationBridge::FreeStates(TArray<FTracked::FStateBuf>& States, UObject* Obj)
{
	if (!Obj || States.Num() == 0) return;
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	FNetSerializationContext Ctx;
	for (int32 i = 0; i < States.Num() && i < Descs.Num(); ++i)
	{
		FTracked::FStateBuf& S = States[i];
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D || S.Internal.Num() == 0) continue;
		FReplicationStateOperations::FreeDynamicState(Ctx, S.Internal.GetData(), D);
		if (S.bHasLast) FReplicationStateOperations::FreeDynamicState(Ctx, S.Last.GetData(), D);
	}
	States.Reset();
}

// The `iris` component, binary form: {"c": class path, "s": [{"i": descriptor index, "bits": n, "b": base64 of
// FReplicationStateOperations::Serialize of the quantized state}]}. Only descriptors without object references
// are exported this way — resolving references needs Iris' internal serialization context, which is engine-private;
// classes whose every descriptor has references fall back to the JSON mirror. Unchanged states (IsEqualQuantizedState
// against the last export) are not re-sent; a message carries only the descriptors that changed.
static bool MemberInMask(const FReplicationStateDescriptor* D, uint32 m, const FNetBitArrayView& Mask)
{
	if (!D->MemberChangeMaskDescriptors) return true;
	const FReplicationStateMemberChangeMaskDescriptor& C = D->MemberChangeMaskDescriptors[m];
	return C.BitCount == 0 || Mask.IsAnyBitSet(C.BitOffset, C.BitCount);
}

bool UMusubiReplicationBridge::ExportBinary(TArray<FTracked::FStateBuf>& TStates, UObject* Obj, TArray<FDescOut>& Out, bool bKeyframe, uint32& OutBytes)
{
	OutBytes = 0;
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	if (Descs.Num() == 0) return false;
	if (TStates.Num() != Descs.Num()) TStates.SetNum(Descs.Num());
	bool bEligible = false;
	for (int32 i = 0; i < Descs.Num(); ++i)
	{
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D || D->MemberCount == 0) continue;
		bEligible = true;
		FTracked::FStateBuf& S = TStates[i];
		const int32 Words = FMath::Max(1, (int32)((D->ChangeMaskBitCount + 31) / 32));
		if (S.External.Num() != (int32)D->ExternalSize)
		{
			S.External.SetNumZeroed(D->ExternalSize);
			S.Internal.SetNumZeroed(D->InternalSize);
			S.Last.SetNumZeroed(D->InternalSize);
			S.Dirty.SetNumZeroed(Words);
			S.bHasLast = false;
		}
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			const FProperty* P = D->MemberProperties ? D->MemberProperties[m] : nullptr;
			if (!P) continue;
			const uint16 ArrayIndex = D->MemberPropertyDescriptors ? D->MemberPropertyDescriptors[m].ArrayIndex : 0;
			P->CopySingleValue(S.External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset, P->ContainerPtrToValuePtr<void>(Obj, ArrayIndex));
		}
		TArray<uint32> ValueStorage;
		const FNetBitArrayView Value = ValueMask(D, ValueStorage);
		TMap<FString, FString> Refs;
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			if (!IsRefMember(D, m) || !D->MemberProperties) continue;
			if (const FObjectPropertyBase* OP = CastField<FObjectPropertyBase>(D->MemberProperties[m]))
				Refs.Add(OP->GetName(), RefId(OP->GetObjectPropertyValue(S.External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset)));
		}
		FNetSerializationContext Ctx;
		Ctx.SetChangeMask(&Value); // the *WithMask operations check the context carries this exact mask
		FReplicationStateOperations::QuantizeWithMask(Ctx, Value, 0, S.Internal.GetData(), S.External.GetData(), D);
		// Member-level diff against the previous tick (serializer IsEqual on the quantized form), accumulated into the
		// dirty mask since the last keyframe so a delta always carries everything a ghost that only has the keyframe lacks.
		bool bAnyMember = !S.bHasLast;
		for (uint32 m = 0; m < D->MemberCount; ++m)
		{
			if (!D->MemberChangeMaskDescriptors || !D->MemberSerializerDescriptors || IsRefMember(D, m) || IsConnectionSpecificMember(D, m) || IsNopMember(D, m)) continue;
			const FReplicationStateMemberSerializerDescriptor& SD = D->MemberSerializerDescriptors[m];
			if (S.bHasLast && SD.Serializer)
			{
				FNetIsEqualArgs A;
				A.Version = 0;
				A.NetSerializerConfig = SD.SerializerConfig;
				A.Source0 = NetSerializerValuePointer(S.Internal.GetData() + D->MemberDescriptors[m].InternalMemberOffset);
				A.Source1 = NetSerializerValuePointer(S.Last.GetData() + D->MemberDescriptors[m].InternalMemberOffset);
				A.bStateIsQuantized = true;
				if (SD.Serializer->IsEqual(Ctx, A)) continue;
			}
			const FReplicationStateMemberChangeMaskDescriptor& C = D->MemberChangeMaskDescriptors[m];
			for (uint16 b = 0; b < C.BitCount; ++b) S.Dirty[(C.BitOffset + b) / 32] |= 1u << ((C.BitOffset + b) % 32);
			bAnyMember = true;
		}
		if (!S.bHasLast || !Refs.OrderIndependentCompareEqual(S.LastRefs)) { S.LastRefs = Refs; S.bRefsDirty = true; }
		if (bAnyMember)
		{
			if (S.bHasLast) FReplicationStateOperations::FreeDynamicState(Ctx, S.Last.GetData(), D);
			FMemory::Memcpy(S.Last.GetData(), S.Internal.GetData(), D->InternalSize);
			S.bHasLast = true;
		}
		else
		{
			FReplicationStateOperations::FreeDynamicState(Ctx, S.Internal.GetData(), D);
		}
		FMemory::Memzero(S.Internal.GetData(), D->InternalSize); // dynamic state now owned by Last (or freed)
		bool bDirtyAny = S.bRefsDirty;
		for (int32 w = 0; w < Words && !bDirtyAny; ++w) bDirtyAny |= S.Dirty[w] != 0;
		if (!bKeyframe && !bDirtyAny) continue;
		// keyframe: every exportable member; delta: the members dirty since the keyframe (the mask travels with the bits)
		TArray<uint32> SendStorage = ValueStorage;
		if (!bKeyframe) for (int32 w = 0; w < Words && w < SendStorage.Num(); ++w) SendStorage[w] &= S.Dirty[w];
		FNetBitArrayView Send(SendStorage.GetData(), D->ChangeMaskBitCount, FNetBitArrayView::NoResetNoValidate);
		uint8 Buf[8192];
		FNetBitStreamWriter W;
		W.InitBytes(Buf, sizeof(Buf));
		FNetSerializationContext WCtx(&W);
		WCtx.SetChangeMask(&Send);
		FReplicationStateOperations::SerializeWithMask(WCtx, Send, 0, S.Last.GetData(), D);
		W.CommitWrites();
		if (W.IsOverflown())
		{
			UE_LOG(LogMusubiIris, Warning, TEXT("%s descriptor %d: state over %d bytes, not exported"), *Obj->GetName(), i, (int32)sizeof(Buf));
			continue;
		}
		const uint32 Bits = W.GetPosBits();
		FDescOut& O = Out.AddDefaulted_GetRef();
		O.Index = i;
		O.Bits = Bits;
		O.Bytes.Append(Buf, (Bits + 7) / 8);
		if (!bKeyframe) for (int32 w = 0; w < Words && w < SendStorage.Num(); ++w) O.Mask.Add(SendStorage[w]);
		if (Refs.Num() && (bKeyframe || S.bRefsDirty)) O.Refs = Refs;
		OutBytes += (Bits + 7) / 8;
		if (bKeyframe)
		{
			FMemory::Memzero(S.Dirty.GetData(), S.Dirty.Num() * sizeof(uint32));
			S.bRefsDirty = false;
		}
	}
	return bEligible;
}

// Blob: u8 version, u8 kind (0 keyframe / 1 delta), u64 tick, [keyframe: class path], descriptors, u8 subs × (name,
// descriptors). One base64 string per message instead of a JSON object per descriptor: the envelope is two quotes.
static void WriteDescs(FArchive& Ar, TArray<UMusubiReplicationBridge::FDescOut>& Descs, bool bKey)
{
	uint8 N = (uint8)FMath::Min(Descs.Num(), 255);
	Ar << N;
	for (int32 d = 0; d < N; ++d)
	{
		UMusubiReplicationBridge::FDescOut& D = Descs[d];
		uint8 I = (uint8)D.Index;
		Ar << I << D.Bits;
		if (!bKey) Ar << D.Mask;
		Ar << D.Bytes;
		uint8 R = (uint8)FMath::Min(D.Refs.Num(), 255);
		Ar << R;
		for (TPair<FString, FString>& KV : D.Refs) { Ar << KV.Key << KV.Value; }
	}
}

static bool ReadDescs(FArchive& Ar, TArray<UMusubiReplicationBridge::FDescOut>& Descs, bool bKey)
{
	uint8 N = 0;
	Ar << N;
	for (int32 d = 0; d < N && !Ar.IsError(); ++d)
	{
		UMusubiReplicationBridge::FDescOut& D = Descs.AddDefaulted_GetRef();
		uint8 I = 0;
		Ar << I << D.Bits;
		D.Index = I;
		if (!bKey) Ar << D.Mask;
		Ar << D.Bytes;
		uint8 R = 0;
		Ar << R;
		for (int32 r = 0; r < R && !Ar.IsError(); ++r) { FString K, V; Ar << K << V; D.Refs.Add(K, V); }
	}
	return !Ar.IsError();
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
	++ExportTick;
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
		TArray<FDescOut> Own;
		const bool bKey = !T.bMirrored || (T.bDirtySinceKey && ExportTick - T.KeyTick >= KeyframeInterval);
		uint32 Bytes = 0;
		if (ExportBinary(T.States, Obj, Own, bKey, Bytes))
		{
			bool bChanged = Own.Num() > 0;
			// replicated components ride along under "sub" by component name (CharacterMovement, inventories, ...);
			// only the ones whose state changed are in the message, like the actor's own descriptors
			TArray<TPair<FString, TArray<FDescOut>>> Subs;
			auto ExportSub = [&](UObject* S)
			{
				TArray<FDescOut> SubOut;
				uint32 SubBytes = 0;
				if (!ExportBinary(T.Subs.FindOrAdd(S->GetFName()), S, SubOut, bKey, SubBytes) || SubOut.Num() == 0) return;
				Bytes += SubBytes;
				Subs.Emplace(S->GetName(), MoveTemp(SubOut));
				bChanged = true;
			};
			if (AActor* A = Cast<AActor>(Obj))
				for (UActorComponent* C : A->GetReplicatedComponents())
					if (C && !C->IsA<UMusubiAuthorityComponent>()) ExportSub(C);
			// registered replicated subobjects (AddReplicatedSubObject + RegisterMusubiSubObject) ride the same way, by name
			for (const TWeakObjectPtr<UObject>& S : Auth->MusubiSubObjects())
				if (UObject* O = S.Get()) ExportSub(O);
			if (bChanged)
			{
				if (!T.bMirrored) UE_LOG(LogMusubiIris, Log, TEXT("%s: first binary iris state exported (%d component(s))"), *Obj->GetName(), Subs.Num());
				T.bMirrored = true;
				TArray<uint8> Blob;
				FMemoryWriter Ar(Blob);
				uint8 Ver = 1, Kind = bKey ? 0 : 1;
				uint64 Tick = ExportTick;
				Ar << Ver << Kind << Tick;
				if (bKey) { FString Class = Obj->GetClass()->GetPathName(); Ar << Class; }
				WriteDescs(Ar, Own, bKey);
				uint8 NSubs = (uint8)FMath::Min(Subs.Num(), 255);
				Ar << NSubs;
				for (int32 sI = 0; sI < NSubs; ++sI) { Ar << Subs[sI].Key; WriteDescs(Ar, Subs[sI].Value, bKey); }
				FMusubiJson Bin;
				Bin.Value = MakeShared<FJsonValueString>(FBase64::Encode(Blob));
				Bytes = Blob.Num(); // what the node carries (before base64)
				if (bKey)
				{
					T.KeyTick = ExportTick;
					T.bDirtySinceKey = false;
					++T.Keys;
					T.KeyBytes += Bytes;
					Auth->SetComponent(TEXT("iris"), Bin);
					if (T.Keys % 10 == 0)
						UE_LOG(LogMusubiIris, Log, TEXT("%s: iris stats keyframes=%u (%llu B, avg %llu) deltas=%u (%llu B, avg %llu)"), *Obj->GetName(), T.Keys, T.KeyBytes, T.KeyBytes / T.Keys, T.Deltas, T.DeltaBytes, T.Deltas ? T.DeltaBytes / T.Deltas : 0);
				}
				else
				{
					T.bDirtySinceKey = true;
					++T.Deltas;
					T.DeltaBytes += Bytes;
					Auth->SetComponent(TEXT("iris.d"), Bin);
				}
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

bool UMusubiReplicationBridge::ApplyDescs(UObject* Obj, const TArray<FDescOut>& In, bool bKeyframe, UMusubiSimulatorSubsystem* Sim)
{
	const TArray<TRefCountPtr<const FReplicationStateDescriptor>>& Descs = DescriptorsFor(Obj->GetClass());
	bool bApplied = false;
	for (const FDescOut& Desc : In)
	{
		const int32 i = Desc.Index;
		const uint32 Bits = Desc.Bits;
		if (!Descs.IsValidIndex(i) || Desc.Bytes.Num() * 8 < (int32)Bits) continue;
		const FReplicationStateDescriptor* D = Descs[i].GetReference();
		if (!D) continue;
		TArray<uint32> MaskStorage;
		FNetBitArrayView Mask = ValueMask(D, MaskStorage);
		if (!bKeyframe) // delta: only the members the owner marked dirty are in the bits
		{
			for (int32 w = 0; w < MaskStorage.Num(); ++w) MaskStorage[w] &= Desc.Mask.IsValidIndex(w) ? Desc.Mask[w] : 0u;
			Mask = FNetBitArrayView(MaskStorage.GetData(), D->ChangeMaskBitCount, FNetBitArrayView::NoResetNoValidate);
		}
		const TMap<FString, FString>* Refs = Desc.Refs.Num() ? &Desc.Refs : nullptr;
		TArray<uint8> Internal, External;
		Internal.SetNumZeroed(D->InternalSize);
		External.SetNumZeroed(D->ExternalSize);
		FNetBitStreamReader R;
		R.InitBits(Desc.Bytes.GetData(), Bits);
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
			if (!IsRefMember(D, m) && !MemberInMask(D, m, Mask)) continue; // not in this delta
			const uint16 ArrayIndex = D->MemberPropertyDescriptors ? D->MemberPropertyDescriptors[m].ArrayIndex : 0;
			void* Dst = P->ContainerPtrToValuePtr<void>(Obj, ArrayIndex);
			uint8* Src = External.GetData() + D->MemberDescriptors[m].ExternalMemberOffset;
			bool bSame = true;
			if (IsRefMember(D, m))
			{
				const FObjectPropertyBase* OP = CastField<FObjectPropertyBase>(P);
				const FString* IdP = Refs ? Refs->Find(P->GetName()) : nullptr;
				if (!OP || !IdP) continue; // not carried: keep what the ghost has
				const FString Id = *IdP;
				UObject* Want = ResolveRef(Sim, Id);
				if (!Id.IsEmpty() && !Want) continue; // the referenced entity is not on this server (yet)
				bSame = OP->GetObjectPropertyValue(Dst) == Want;
				OP->SetObjectPropertyValue(Dst, Want);
			}
			else
			{
				bSame = P->Identical(Dst, Src);
				if (!ApplyFastArrayFromIris(P, Dst, Src, bSame)) P->CopySingleValue(Dst, Src);
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

// A FastArray written through Iris carries the owner's items but not the classic driver's bookkeeping: the
// replication key is unchanged and the items keep foreign ReplicationIDs, so the GameNetDriver never resends them
// to this server's clients (seen live: the new owner restored inv=4, its client stayed at inv=0). Forget the ids
// (items with INDEX_NONE are re-registered on the next delta serialize) and bump the array key.
static bool ApplyFastArrayFromIris(const FProperty* P, void* Dst, const void* Src, bool bSame)
{
	const FStructProperty* SP = CastField<FStructProperty>(P);
	if (!SP || !SP->Struct || !SP->Struct->IsChildOf(FFastArraySerializer::StaticStruct())) return false;
	if (bSame) return true;
	// Only the game's own fields (the item array) come from Iris. FFastArraySerializer's bookkeeping (IDCounter,
	// ArrayReplicationKey, ItemMap — UPROPERTYs, so a whole-struct copy would clobber them with the scratch
	// buffer's defaults) belongs to this server's classic GameNetDriver and stays. Imported items carry the old
	// owner's ReplicationIDs: forget them (INDEX_NONE items are re-registered on the next delta serialize) and
	// bump the array key so every connection resends the array (seen live: the new owner restored inv=4, its
	// client stayed at inv=0, then never received later items).
	FFastArraySerializer* FA = static_cast<FFastArraySerializer*>(Dst);
	int32 Items = 0;
	for (TFieldIterator<FProperty> It(SP->Struct); It; ++It)
	{
		if (It->GetOwnerStruct() == FFastArraySerializer::StaticStruct()) continue;
		It->CopyCompleteValue(It->ContainerPtrToValuePtr<void>(Dst), It->ContainerPtrToValuePtr<void>(Src));
		const FArrayProperty* AP = CastField<FArrayProperty>(*It);
		const FStructProperty* Inner = AP ? CastField<FStructProperty>(AP->Inner) : nullptr;
		if (!Inner || !Inner->Struct || !Inner->Struct->IsChildOf(FFastArraySerializerItem::StaticStruct())) continue;
		FScriptArrayHelper H(AP, AP->ContainerPtrToValuePtr<void>(Dst));
		for (int32 i = 0; i < H.Num(); ++i)
		{
			FFastArraySerializerItem* Item = reinterpret_cast<FFastArraySerializerItem*>(H.GetRawPtr(i));
			Item->ReplicationID = INDEX_NONE;
			Item->ReplicationKey = INDEX_NONE;
		}
		Items += H.Num();
	}
	const int32 KeyBefore = FA->ArrayReplicationKey;
	FA->MarkArrayDirty();
	UE_LOG(LogMusubiIris, Log, TEXT("%s: %d FastArray item(s) imported from iris (key %d -> %d, id counter %d)"), *P->GetName(), Items, KeyBefore, FA->ArrayReplicationKey, FA->IDCounter);
	return true;
}

bool UMusubiReplicationBridge::ApplyBinary(UObject* Obj, const FMusubiJson& Value, UMusubiSimulatorSubsystem* Sim)
{
	FString B64;
	if (!Obj || !Value.Value.IsValid() || !Value.Value->TryGetString(B64)) return false; // JSON mirror form, not ours
	TArray<uint8> Blob;
	if (!FBase64::Decode(B64, Blob)) return false;
	FMemoryReader Ar(Blob);
	uint8 Ver = 0, Kind = 0;
	uint64 Tick = 0;
	Ar << Ver << Kind << Tick;
	if (Ar.IsError() || Ver != 1) { UE_LOG(LogMusubiIris, Warning, TEXT("%s: bad iris blob (version %u)"), *Obj->GetName(), Ver); return false; }
	const bool bKey = Kind == 0;
	if (bKey)
	{
		FString ClassPath;
		Ar << ClassPath;
		if (ClassPath != Obj->GetClass()->GetPathName())
		{
			UE_LOG(LogMusubiIris, Warning, TEXT("%s: iris state is for %s, ghost is %s — skipped"), *Obj->GetName(), *ClassPath, *Obj->GetClass()->GetPathName());
			return false;
		}
	}
	uint64& Last = AppliedTick.FindOrAdd(Obj);
	if (Tick < Last) return false; // an older keyframe arriving after a newer delta
	Last = Tick;
	if (bKey) UE_LOG(LogMusubiIris, Log, TEXT("%s: iris keyframe t=%llu applied (%d bytes)"), *Obj->GetName(), Tick, Blob.Num());
	TArray<FDescOut> Own;
	if (!ReadDescs(Ar, Own, bKey)) return false;
	bool bApplied = ApplyDescs(Obj, Own, bKey, Sim);
	uint8 NSubs = 0;
	Ar << NSubs;
	AActor* A = Cast<AActor>(Obj);
	for (int32 sI = 0; sI < NSubs && !Ar.IsError(); ++sI)
	{
		FString Name;
		Ar << Name;
		TArray<FDescOut> Sub;
		if (!ReadDescs(Ar, Sub, bKey)) break;
		UObject* Target = nullptr;
		if (A) for (UActorComponent* C : A->GetComponents()) if (C && C->GetName() == Name) { Target = C; break; }
		if (!Target) if (UMusubiAuthorityComponent* Auth = AuthorityOf(Obj)) Target = Auth->FindMusubiSubObject(Name);
		if (!Target) { UE_LOG(LogMusubiIris, Warning, TEXT("%s: iris state for component/subobject %s, this side has none — skipped"), *Obj->GetName(), *Name); continue; }
		bApplied |= ApplyDescs(Target, Sub, bKey, Sim);
	}
	return bApplied;
}
