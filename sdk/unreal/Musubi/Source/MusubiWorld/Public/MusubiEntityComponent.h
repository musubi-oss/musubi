// Attached to every actor the world view spawns: the entity id and its components as JSON, with change events.
#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "GameFramework/Actor.h"
#include "MusubiWorldTypes.h"
#include "MusubiEntityComponent.generated.h"

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnEntityComponentChanged, const FString&, Name, const FString&, ValueJson);
DECLARE_DYNAMIC_MULTICAST_DELEGATE(FMusubiOnEntityEvent);

UCLASS(ClassGroup = (Musubi), meta = (BlueprintSpawnableComponent))
class MUSUBIWORLD_API UMusubiEntityComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") FString GetEntityId() const { return EntityId.ToString(); }
	const FMusubiUuid& GetEntityUuid() const { return EntityId; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") bool IsLocalPlayer() const { return bLocal; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") bool HasComponent(const FString& Name) const { return Components.Contains(Name); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") FString GetComponentJson(const FString& Name) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") FString GetComponentString(const FString& Name, const FString& Default = "") const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") double GetComponentNumber(const FString& Name, double Default = 0.0) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") TArray<FString> ComponentNames() const;
	const FMusubiJson* FindComponent(const FString& Name) const;
	/** Authoritative tick of the last frame that touched this entity. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Entity") int64 GetLastTick() const { return (int64)LastTick; }

	UPROPERTY(BlueprintAssignable, Category = "Musubi|Entity") FMusubiOnEntityComponentChanged OnComponentChanged;
	/** Fired after spawn (or re-sync) once all components are set. */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Entity") FMusubiOnEntityEvent OnEntityReady;
	/** Fired right before the actor goes back to the pool / is destroyed. */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Entity") FMusubiOnEntityEvent OnEntityDespawned;

	// Driven by UMusubiWorldComponent.
	void Reset(const FMusubiUuid& Id, bool bIsLocal);
	void SetComponent(const FString& Name, const FMusubiJson& Value, uint64 Version);
	void SetLastTick(uint64 Tick) { LastTick = Tick; }
	void NotifyReady() { OnEntityReady.Broadcast(); }
	void NotifyDespawned() { OnEntityDespawned.Broadcast(); }

private:
	FMusubiUuid EntityId;
	bool bLocal = false;
	uint64 LastTick = 0;
	TMap<FString, FMusubiEntityComponentState> Components;
};

/** Convenience base: an empty actor with a UMusubiEntityComponent, for Blueprint subclasses. */
UCLASS(Blueprintable)
class MUSUBIWORLD_API AMusubiEntityActor : public AActor
{
	GENERATED_BODY()
public:
	AMusubiEntityActor();
	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category = "Musubi") TObjectPtr<UMusubiEntityComponent> Entity;
};
