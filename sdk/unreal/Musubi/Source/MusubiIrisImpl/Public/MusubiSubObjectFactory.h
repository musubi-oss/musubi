#pragma once
#include "CoreMinimal.h"
#include "Runtime/Launch/Resources/Version.h"
#include "Net/Iris/ReplicationSystem/NetSubObjectFactory.h"
#include "MusubiSubObjectFactory.generated.h"

/**
 * Iris subobject factory for actors kept across an in-place rehome (5.7+): the engine's subobject factory, except that
 * a dynamic (registered, non-component) subobject of a kept actor is matched to the instance the actor already owns
 * (same outer, same class, registered, not yet replicated) instead of creating a new one, and kept actors' subobjects
 * survive the old connection's detach.
 */
UCLASS(MinimalAPI)
class UMusubiSubObjectFactory : public UNetSubObjectFactory
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
#endif
};
