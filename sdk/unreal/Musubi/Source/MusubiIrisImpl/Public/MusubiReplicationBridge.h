#pragma once

#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"

#include "Iris/ReplicationSystem/ObjectReplicationBridge.h"
#include "Iris/ReplicationState/ReplicationStateDescriptor.h"
#include "Iris/ReplicationSystem/ReplicationBridgeTypes.h"

class FJsonObject;

#include "MusubiReplicationBridge.generated.h"

class UMusubiSimulatorSubsystem;
class UMusubiAuthorityComponent;

/**
 * Tier B (5.5+): Iris → Musubi bridge. This directory (MusubiIrisImpl) is not a module: MusubiIris.Build.cs
 * adds it with ConditionalAddModuleDirectory on 5.5+ only, so UHT never sees these classes on 5.3/5.4
 * (UHT skips `#if MACRO … #else … #endif` around a base class entirely, leaving `Super` undefined).
 *
 * Iris stays the owner of dirty tracking / replication state descriptors; this bridge sits
 * on top and, once per send update, walks the root objects that Iris marked dirty, serializes
 * their replicated properties through FMusubiNetSerializer and hands the result to
 * UMusubiSimulatorSubsystem as `apply` components (the node fans them out to interested
 * clients / other simulators). Detach from Iris == despawn in Musubi.
 *
 * Verified against 5.7.4 / 5.8.4 ObjectReplicationBridge.h:
 *  - OnStartPreSendUpdate / PreSendUpdate / UpdateInstancesWorldLocation exist in both;
 *  - OnProtocolMismatchReported gained `const TArray<uint8>& ClientCDOStateBytes` in 5.8;
 *  - DetachInstance(FNetRefHandle) is virtual in 5.7 and gone in 5.8. The 5.8 detach delegates
 *    (FObjectReplicationBridgeDelegates::Get*DetachedDelegate) are private + friend-only (engine
 *    classes), so a plugin bridge cannot subscribe (UBT 5.8.4: "use of undeclared identifier"); on 5.8
 *    detach is detected at the start of the next send update instead: a tracked handle whose
 *    GetReplicatedObject() is null has been detached → Untrack (one update late, same outcome).
 */
UCLASS(Transient, MinimalAPI)
class UMusubiReplicationBridge : public UObjectReplicationBridge
{
	GENERATED_BODY()
public:
	UMusubiReplicationBridge();

	virtual void Initialize(UReplicationSystem* InReplicationSystem) override;
	virtual void Deinitialize() override;

	virtual void OnStartPreSendUpdate() override;
	virtual void PreSendUpdate() override;
	virtual void UpdateInstancesWorldLocation() override;
	virtual bool CacheNetRefHandleCreationInfo(UE::Net::FNetRefHandle Handle) override;
	virtual void PruneStaleObjects() override;

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	virtual void OnProtocolMismatchReported(UE::Net::FNetRefHandle RefHandle, uint32 ConnectionId, const TArray<uint8>& ClientCDOStateBytes = TArray<uint8>()) override;
#else
	virtual void OnProtocolMismatchReported(UE::Net::FNetRefHandle RefHandle, uint32 ConnectionId) override;
	virtual void DetachInstance(UE::Net::FNetRefHandle Handle) override;
#endif

	/** Objects that carry a UMusubiAuthorityComponent and are owned by this simulator. */
	int32 NumTrackedObjects() const { return Tracked.Num(); }
	// Begin mirroring a root object the export subsystem just registered. CacheNetRefHandleCreationInfo only
	// fires when a creation header is built for a real connection; our export system has none, so the owner
	// of the handle has to tell us explicitly.
	void Track(UE::Net::FNetRefHandle Handle, UObject* Object);

private:
	struct FTracked
	{
		bool bMirrored = false;
		TWeakObjectPtr<UObject> Object;
		TWeakObjectPtr<UMusubiAuthorityComponent> Authority;
		uint64 LastHash = 0;
		struct FStateBuf { TArray<uint8> External, Internal, Last; TMap<FString, FString> LastRefs; bool bHasLast = false; };
		TArray<FStateBuf> States; // one per class descriptor (binary export)
	};
	/** Class → Iris replication state descriptors built once (CDO as default state). */
	TMap<TWeakObjectPtr<UClass>, TArray<TRefCountPtr<const UE::Net::FReplicationStateDescriptor>>> ClassDescriptors;
	const TArray<TRefCountPtr<const UE::Net::FReplicationStateDescriptor>>& DescriptorsFor(UClass* Class);
	/** Binary path: quantize + serialize each eligible descriptor state; false = nothing eligible (JSON fallback). */
	bool ExportBinary(FTracked& T, UObject* Obj, TSharedPtr<FJsonObject>& Out, bool& bChanged);
	void FreeStates(FTracked& T, UObject* Obj);

public:
	/** Ghost side: deserialize + dequantize an `iris` component (binary form) into Obj's UPROPERTYs, firing RepNotifies. */
	bool ApplyBinary(UObject* Obj, const TSharedPtr<FJsonObject>& Iris, class UMusubiSimulatorSubsystem* Sim);

	UMusubiSimulatorSubsystem* Simulator() const;
	void Untrack(UE::Net::FNetRefHandle Handle, UObject* Object); // EDetachReason exists only since 5.8, so the reason is not part of the signature
	void FlushDirty();

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	void SweepDetached();
#endif

	TMap<UE::Net::FNetRefHandle, FTracked> Tracked;
	TSet<UE::Net::FNetRefHandle> DirtyThisUpdate;
	bool bInSendUpdate = false;
};
