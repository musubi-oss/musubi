// Stable handle from a Musubi entity to whatever actor currently stands for it in this world. Actor pointers are not
// stable across a server change: on the client a rehome replaces the actor with the new owner's copy (dynamic NetGUIDs
// cannot re-attach to an existing actor), on a server a ghost comes and goes with the neighbouring cell. Gameplay code
// keeps the entity id (FMusubiEntityHandle) and resolves it when it needs the actor, or listens to OnEntityActorChanged.
#pragma once

#include "CoreMinimal.h"
#include "Subsystems/WorldSubsystem.h"
#include "MusubiEntityRegistry.generated.h"

USTRUCT(BlueprintType)
struct MUSUBISERVER_API FMusubiEntityHandle
{
	GENERATED_BODY()
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi") FString EntityId;
	bool IsSet() const { return !EntityId.IsEmpty(); }
	/** The actor that stands for the entity in the world of WorldContext right now, or null (not in view / not spawned yet). */
	AActor* Get(const UObject* WorldContext) const;
	bool operator==(const FMusubiEntityHandle& O) const { return EntityId == O.EntityId; }
};

DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnEntityActorChanged, const FString&, EntityId, AActor*, OldActor, AActor*, NewActor);
DECLARE_MULTICAST_DELEGATE_ThreeParams(FMusubiOnEntityActorChangedNative, const FString& /*EntityId*/, AActor* /*Old*/, AActor* /*New*/);

/** Per world (client and server): entity id → current actor. Filled by UMusubiAuthorityComponent, never by hand. */
UCLASS()
class MUSUBISERVER_API UMusubiEntityRegistry : public UWorldSubsystem
{
	GENERATED_BODY()
public:
	static UMusubiEntityRegistry* Get(const UObject* WorldContext);

	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") AActor* Resolve(const FString& EntityId) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity", meta = (WorldContext = "WorldContext")) static AActor* ResolveEntity(const UObject* WorldContext, const FString& EntityId);
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity", meta = (WorldContext = "WorldContext")) static AActor* ResolveHandle(const UObject* WorldContext, const FMusubiEntityHandle& Handle);
	/** Handle of an actor that stands for a Musubi entity (has an identified UMusubiAuthorityComponent); unset otherwise. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") static FMusubiEntityHandle HandleOf(AActor* Actor);
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") static bool IsHandleSet(const FMusubiEntityHandle& Handle) { return Handle.IsSet(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") TArray<FString> EntityIds() const;

	/** The actor standing for an entity was replaced while the old one was still alive (client rehome swap). Fired before
	 *  the old actor is destroyed, so references can be moved over. */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Entity") FMusubiOnEntityActorChanged OnEntityActorChanged;
	FMusubiOnEntityActorChangedNative OnEntityActorChangedNative;
	int32 ChangeCount() const { return NumChanges; }

	void Register(const FString& EntityId, AActor* Actor);
	void Unregister(const FString& EntityId, AActor* Actor);

private:
	TMap<FString, TWeakObjectPtr<AActor>> Actors;
	int32 NumChanges = 0;
};
