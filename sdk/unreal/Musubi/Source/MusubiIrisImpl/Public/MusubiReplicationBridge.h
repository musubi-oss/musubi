#pragma once

#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"

#include "Iris/ReplicationSystem/ObjectReplicationBridge.h"
#include "Iris/ReplicationSystem/ReplicationBridgeTypes.h"

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
 *  - DetachInstance(FNetRefHandle) is virtual in 5.7, gone in 5.8 → delegates
 *    GetRootObjectDetachedDelegate / GetSubObjectDetachedDelegate instead.
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

private:
	struct FTracked
	{
		TWeakObjectPtr<UObject> Object;
		TWeakObjectPtr<UMusubiAuthorityComponent> Authority;
		uint64 LastHash = 0;
	};

	UMusubiSimulatorSubsystem* Simulator() const;
	void Track(UE::Net::FNetRefHandle Handle, UObject* Object);
	void Untrack(UE::Net::FNetRefHandle Handle, UObject* Object); // EDetachReason exists only since 5.8, so the reason is not part of the signature
	void FlushDirty();

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	FDelegateHandle RootDetachedHandle;
	FDelegateHandle SubObjectDetachedHandle;
#endif

	TMap<UE::Net::FNetRefHandle, FTracked> Tracked;
	TSet<UE::Net::FNetRefHandle> DirtyThisUpdate;
	bool bInSendUpdate = false;
};
