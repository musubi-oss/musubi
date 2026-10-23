#pragma once

#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"
#include "MusubiPostcard.h"

#include "Iris/ReplicationSystem/NetObjectFactory.h"

#include "MusubiNetObjectFactory.generated.h"

/**
 * Creation header for Musubi-owned replicated objects: the Unreal class to instantiate on the
 * remote side plus the Musubi entity id, so a ghost spawned via Iris and the same entity seen
 * through the Musubi world view resolve to one object. Verified against 5.7 NetObjectFactory.h
 * (FNetObjectCreationHeader: virtual dtor, Set/GetProtocolId, Set/GetNetFactoryId).
 */
class FMusubiCreationHeader : public UE::Net::FNetObjectCreationHeader
{
public:
	FString ClassPath;
	FMusubiUuid Entity;
	bool bIsActor = false;
};

/** Registered under name "MusubiEntity" (UE::Net::FNetObjectFactoryRegistry::RegisterFactory). */
UCLASS(MinimalAPI)
class UMusubiNetObjectFactory : public UNetObjectFactory
{
	GENERATED_BODY()
public:
	static const FName FactoryName;

	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndFillHeader(UE::Net::FNetRefHandle Handle) override;
	virtual bool SerializeHeader(const UE::Net::FCreationHeaderContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndDeserializeHeader(const UE::Net::FCreationHeaderContext& Context) override;
	virtual FInstantiateResult InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
	virtual void PostInstantiation(const FPostInstantiationContext& Context) override;
	// PURE_VIRTUAL in the engine = compiles without, fatal at the first call (seen live on 5.8: FillRootObjectReplicationParams)
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION <= 6
	// 5.5/5.6: out-parameter world info, poll frequency not a factory concern, destruction via DestroyReplicatedObject
	virtual void GetWorldInfo(const FWorldInfoContext& Context, FWorldInfoData& OutData) override;
	virtual void DestroyReplicatedObject(const FDestroyedContext& Context) override;
#else
	virtual TOptional<FWorldInfoData> GetWorldInfo(const FWorldInfoContext& Context) const override;
#if ENGINE_MINOR_VERSION >= 8
	virtual void FillRootObjectReplicationParams(const UE::Net::FRootObjectReplicationParamsContext& Context, UE::Net::FRootObjectReplicationParams& OutParams) override;
	// 5.8: FDestroyedContext is deprecated, the pure virtual is the FDetachContext overload
	virtual void DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext) override;
#else
	virtual float GetPollFrequency(UE::Net::FNetRefHandle RootObjectHandle, UObject* RootObjectInstance) override { return 100.0f; }
	virtual void DetachedFromReplication(const FDestroyedContext& Context) override;
#endif
#endif
};
