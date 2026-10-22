#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "MassEntityTypes.h"
#include "MassArchetypeTypes.h"
#include "MusubiWireTypes.h"
#include "MusubiWorldTypes.h"
#include "MusubiMassSinkComponent.generated.h"

class UMusubiWorldComponent;
struct FMassEntityManager;

/** Fired on the game thread right after the Mass entity exists; add project fragments here (`Manager.Defer().AddFragment<...>`). */
DECLARE_MULTICAST_DELEGATE_ThreeParams(FMusubiMassEntityEvent, FMassEntityManager& /*Manager*/, FMassEntityHandle /*Entity*/, const FMusubiEntityRecord& /*Record*/);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiMassEntityGoneEvent, FMassEntityManager& /*Manager*/, FMassEntityHandle /*Entity*/);

/**
 * Mass sink of a UMusubiWorldComponent: every entity whose mapping rule says `Sink = Mass` gets a Mass entity
 * (archetype FTransformFragment + FMusubiEntityFragment + FMusubiEntityTag). Transforms follow the world
 * component's interpolated pose each frame (this component ticks after it); components live on the record and
 * are announced by `FMusubiEntityFragment::ComponentsRevision`. Put it on the same actor as the world component
 * or point `World` at one.
 */
UCLASS(ClassGroup = (Musubi), meta = (BlueprintSpawnableComponent))
class MUSUBIMASS_API UMusubiMassSinkComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UMusubiMassSinkComponent();

	/** World component to mirror (default: the first one on the owner). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|Mass") TObjectPtr<UMusubiWorldComponent> World;
	/** Component carrying the yaw in degrees (JSON number); empty or missing → face the movement direction. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|Mass") FString YawComponent = TEXT("yaw");
	/** Uniform scale written into the transform fragment. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|Mass", meta = (ClampMin = "0.001")) float Scale = 1.f;

	UFUNCTION(BlueprintPure, Category = "Musubi|Mass") int32 MassEntityCount() const { return Entities.Num(); }
	bool FindMassEntity(const FMusubiUuid& Id, FMassEntityHandle& Out) const;
	/** Reverse lookup for processors that only have the Mass handle. */
	bool FindRecordId(FMassEntityHandle Entity, FMusubiUuid& Out) const;
	FMassArchetypeHandle GetArchetype() const { return Archetype; }

	FMusubiMassEntityEvent OnMassEntityCreated;
	FMusubiMassEntityGoneEvent OnMassEntityDestroying;

	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

private:
	struct FTracked
	{
		FMassEntityHandle Entity;
		FVector LastPos = FVector::ZeroVector;
		float Yaw = 0.f;
		uint32 ComponentsHash = 0;
		uint32 Revision = 0;
	};
	TMap<FMusubiUuid, FTracked> Entities;
	TMap<FMassEntityHandle, FMusubiUuid> Reverse;
	FMassArchetypeHandle Archetype;
	TWeakObjectPtr<UMusubiWorldComponent> Bound;
	FDelegateHandle SpawnedHandle, DespawnedHandle, ResetHandle;

	FMassEntityManager* Manager() const;
	bool EnsureArchetype(FMassEntityManager& M);
	void HandleSpawned(const FMusubiEntityRecord& R, bool bSpawn);
	void HandleDespawned(const FMusubiUuid& Id);
	void HandleReset();
	void Destroy(FMassEntityManager& M, const FMusubiUuid& Id, FTracked& T);
	void WriteFragments(FMassEntityManager& M, const FMusubiEntityRecord& R, FTracked& T, bool bForce);
	static uint32 HashComponents(const FMusubiEntityRecord& R);
};
