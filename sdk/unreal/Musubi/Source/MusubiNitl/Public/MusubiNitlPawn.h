#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "MusubiNitlPawn.generated.h"

/** The entity's actor in the node-in-the-loop run: replicated, walks +X on its owner at `-MusubiNitlWalk=<units/s>`. */
UCLASS()
class MUSUBINITL_API AMusubiNitlPawn : public AActor
{
	GENERATED_BODY()
public:
	AMusubiNitlPawn();
	virtual void Tick(float DeltaSeconds) override;
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;

	UPROPERTY(VisibleAnywhere, Category = "Musubi") TObjectPtr<class UMusubiAuthorityComponent> Authority;
	/** Ticks walked on the owner: a client that keeps this actor across a rehome must see it keep counting up. */
	UPROPERTY(ReplicatedUsing = OnRep_Steps) int32 Steps = 0;
	UFUNCTION() void OnRep_Steps();
	float Walk = 0.f;
	double LastLog = 0;
};
