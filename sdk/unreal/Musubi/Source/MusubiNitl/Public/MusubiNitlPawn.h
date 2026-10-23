#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Character.h"
#include "MusubiNitlPawn.generated.h"

/** The entity's actor in the node-in-the-loop run: a replicated Character the client possesses and drives +X (flying, no floor on the Entry map). */
UCLASS()
class MUSUBINITL_API AMusubiNitlPawn : public ACharacter
{
	GENERATED_BODY()
public:
	AMusubiNitlPawn();
	virtual void Tick(float DeltaSeconds) override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void PossessedBy(AController* NewController) override;
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;

	UPROPERTY(VisibleAnywhere, Category = "Musubi") TObjectPtr<class UMusubiAuthorityComponent> Authority;
	/** Ticks walked on the owner: a client that keeps this actor across a rehome must see it keep counting up. */
	UPROPERTY(ReplicatedUsing = OnRep_Steps) int32 Steps = 0;
	UFUNCTION() void OnRep_Steps();
	/** Same counter, but push-model (dirty only via MARK_PROPERTY_DIRTY): proves the classic driver still sees push-based
	 *  dirtiness on an actor the Musubi Iris export system has registered (push ids are a single slot shared by both). */
	UPROPERTY(Replicated) int32 PushSteps = 0;
	double LastLog = 0;
};
