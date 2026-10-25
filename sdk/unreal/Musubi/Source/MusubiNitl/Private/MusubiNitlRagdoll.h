// NITL_RAGDOLL=1: a server-only ragdoll (the engine's TutorialTPP skeletal mesh + its physics asset, every bone a Chaos
// body) kicked across the cell border; the receiving server must resume it from the per-body state the old owner captured.
#pragma once
#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "MusubiNitlRagdoll.generated.h"

UCLASS()
class AMusubiNitlRagdoll : public AActor
{
	GENERATED_BODY()
public:
	AMusubiNitlRagdoll();
	virtual void BeginPlay() override;
	virtual void Tick(float Dt) override;

	UPROPERTY() TObjectPtr<class USkeletalMeshComponent> Mesh;
	UPROPERTY() TObjectPtr<class UMusubiAuthorityComponent> Authority;
	double NextLog = 0;
};
