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

	/** Iris: name of the Musubi NetObjectFactory (MusubiIris registers it). An actor that returns it from
	 *  `FillReplicationParams` (`OutParams.NetFactoryName = UMusubiEntityRegistry::IrisNetFactoryName`) is created on a
	 *  client by entity id: an actor already registered here under that id is reused instead of spawning a new one, so
	 *  a rehome to another server keeps the same AActor* without the classic NetGUID table. */
	static const FName IrisNetFactoryName;
	/** Iris: name of the Musubi subobject factory (dynamic subobjects of kept actors are matched, not re-created). */
	static const FName IrisSubObjectFactoryName;
	/** Set by the client net driver around an in-place rehome: the Musubi factory then keeps entity actors the engine
	 *  detaches with the old connection (the new owner re-attaches to them) instead of destroying them. */
	void SetRehoming(bool bOn);
	/** True only while the client net driver tears down the old connection: the Musubi Iris factories then keep the
	 *  detached entity actors / player actors (and their subobjects) instead of destroying them. */
	void SetKeepingOnDetach(bool bOn) { bKeepOnDetach = bOn; }
	bool IsKeepingOnDetach() const { return bKeepOnDetach; }
	/** The factory took a kept player actor for the new owner's controller/state. */
	void NoteRehomeKeptPlayerReused(const AActor* A);
	bool RehomeKeptPlayerPending() const { return RehomeKeptPC.IsValid() || RehomeKeptPS.IsValid(); }
	/** The player's controller/state kept across the rehome: the Musubi actor factory hands them to the new owner's
	 *  PlayerController/PlayerState creation instead of spawning new ones. */
	void SetRehomeKeptPlayer(AActor* PC, AActor* PS);
	AActor* RehomeKeptPlayerActor(const UClass* Cls) const;
	bool IsRehomeKeptPlayerActor(const AActor* A) const { return A && (A == RehomeKeptPC.Get() || A == RehomeKeptPS.Get()); }
	bool IsRehoming() const { return bRehoming; }
	void NoteIrisKept() { ++IrisKept; }
	void NoteIrisReused() { ++IrisReused; }
	int32 IrisKeptCount() const { return IrisKept; }
	int32 IrisReusedCount() const { return IrisReused; }

	void Register(const FString& EntityId, AActor* Actor);
	void Unregister(const FString& EntityId, AActor* Actor);

private:
	TMap<FString, TWeakObjectPtr<AActor>> Actors;
	int32 NumChanges = 0;
	bool bRehoming = false, bKeepOnDetach = false;
	TWeakObjectPtr<AActor> RehomeKeptPC, RehomeKeptPS;
	int32 IrisKept = 0, IrisReused = 0;
};
