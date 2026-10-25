#pragma once
#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"
#include "MusubiWireTypes.h"
#include "Net/Iris/ReplicationSystem/NetActorFactory.h"
#include "MusubiActorFactory.generated.h"

/**
 * Iris factory for Musubi entity actors on the engine's own replication bridge (server → client, 5.7+).
 *
 * The engine's actor factory plus one field: the entity id. An actor opts in with
 * `FillReplicationParams` → `OutParams.NetFactoryName = UMusubiEntityRegistry::IrisNetFactoryName`. On the client the
 * factory looks the entity up in UMusubiEntityRegistry first and hands the engine the actor it already has — after an
 * in-place rehome the new owner's state lands on the same AActor* (no twin, no swap); only unknown entities are spawned,
 * exactly as UNetActorFactory would. Everything else (archetype, spawn info, OnActorChannelOpen custom data, roles,
 * BeginPlay, tear-off, destroy) is the engine's code.
 */
class FMusubiActorNetCreationHeader : public UE::Net::FDynamicActorNetCreationHeader
{
public:
	FMusubiUuid Entity;
	virtual bool Serialize(const UE::Net::FCreationHeaderContext& Context) const override;
	virtual bool Deserialize(const UE::Net::FCreationHeaderContext& Context) override;
	virtual FString ToString() const override;
};

UCLASS(MinimalAPI)
class UMusubiActorFactory : public UNetActorFactory
{
	GENERATED_BODY()
public:
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
	virtual FInstantiateResult InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
#if ENGINE_MINOR_VERSION >= 8
	virtual void DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext) override;
#else
	virtual void DetachedFromReplication(const FDestroyedContext& Context) override;
#endif
protected:
	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndFillHeader(UE::Net::FNetRefHandle Handle) override;
	virtual TUniquePtr<UE::Net::FNetObjectCreationHeader> CreateAndDeserializeHeader(const UE::Net::FCreationHeaderContext& Context) override;
	virtual bool SerializeHeader(const UE::Net::FCreationHeaderContext& Context, const UE::Net::FNetObjectCreationHeader* Header) override;
#endif
};
