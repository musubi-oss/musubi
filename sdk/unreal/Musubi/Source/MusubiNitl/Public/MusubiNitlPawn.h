#pragma once

#include "CoreMinimal.h"
#include "Misc/EngineVersionComparison.h"
#include "GameFramework/Character.h"
#include "MusubiNitlPawn.generated.h"

/** The entity's actor in the node-in-the-loop run: a replicated Character the client possesses and drives +X (flying, no floor on the Entry map). */
UCLASS()
class MUSUBINITL_API AMusubiNitlPawn : public ACharacter
{
	GENERATED_BODY()
public:
	AMusubiNitlPawn();
	virtual void BeginPlay() override;
	virtual void Tick(float DeltaSeconds) override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void PossessedBy(AController* NewController) override;
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;
#if UE_WITH_IRIS && ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
	/** Iris: replicate through the Musubi NetObjectFactory (created by entity id on clients, same actor across a rehome). */
	virtual void FillReplicationParams(const FFillReplicationParamsContext& Context, FActorReplicationParams& OutParams) override;
#endif

	UPROPERTY(VisibleAnywhere, Category = "Musubi") TObjectPtr<class UMusubiAuthorityComponent> Authority;
	UPROPERTY(VisibleAnywhere, Category = "Musubi")
	TObjectPtr<class UMusubiNitlInventoryComponent> Inventory;
	/** Ticks walked on the owner: a client that keeps this actor across a rehome must see it keep counting up. */
	/** Replicated subobject (not a component): created in BeginPlay on every server (owner and ghost alike) and registered
	 *  with the authority component; the client gets the server's instance through this replicated pointer and registers
	 *  it in the RepNotify, so the same registry read works on every side. */
	UPROPERTY(ReplicatedUsing = OnRep_Loadout) TObjectPtr<class UMusubiNitlLoadout> Loadout;
	/** Client-local life of this very instance: a looping timer started in BeginPlay. A rehome that keeps the same AActor*
	 *  keeps it counting; a swapped-in copy starts from 0 again. Never replicated. */
	int32 LocalTicks = 0;
	FTimerHandle LocalTimer;
	UFUNCTION() void OnRep_Loadout();
	UPROPERTY(ReplicatedUsing = OnRep_Steps) int32 Steps = 0;
	UFUNCTION() void OnRep_Steps();
	/** Same counter, but push-model (dirty only via MARK_PROPERTY_DIRTY): proves the classic driver still sees push-based
	 *  dirtiness on an actor the Musubi Iris export system has registered (push ids are a single slot shared by both). */
	UPROPERTY(Replicated) int32 PushSteps = 0;
	double LastLog = 0;
};
