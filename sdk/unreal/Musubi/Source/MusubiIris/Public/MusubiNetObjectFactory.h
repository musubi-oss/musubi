#pragma once

#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"
#include "MusubiPostcard.h"

#if MUSUBI_WITH_IRIS
#include "Iris/ReplicationSystem/NetObjectFactory.h"
#endif

#include "MusubiNetObjectFactory.generated.h"

#if MUSUBI_WITH_IRIS
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
#endif

/** Registered under name "MusubiEntity" (UE::Net::FNetObjectFactoryRegistry::RegisterFactory). */
UCLASS(MinimalAPI)
class UMusubiNetObjectFactory
#if MUSUBI_WITH_IRIS
	: public UNetObjectFactory
#else
	: public UObject
#endif
{
	GENERATED_BODY()
public:
	static const FName FactoryName;

#if MUSUBI_WITH_IRIS
	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndFillHeader(UE::Net::FNetRefHandle Handle) override;
	virtual bool SerializeHeader(const UE::Net::FCreationHeaderContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndDeserializeHeader(const UE::Net::FCreationHeaderContext& Context) override;
	virtual FInstantiateResult InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
	virtual void PostInstantiation(const FPostInstantiationContext& Context) override;
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
	// 5.8: FDestroyedContext is deprecated, the pure virtual is the FDetachContext overload
	virtual void DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext) override;
#else
	virtual void DetachedFromReplication(const FDestroyedContext& Context) override;
#endif
#endif
};
