#pragma once

#include "CoreMinimal.h"
#include "UObject/Object.h"
#include "MusubiNitlLoadout.generated.h"

/** A replicated subobject (`AddReplicatedSubObject`, not a component) on the NITL pawn: its state must ride the handover
 *  through `RegisterMusubiSubObject` and reach the client of the new owner through the classic driver. */
UCLASS()
class MUSUBINITL_API UMusubiNitlLoadout : public UObject
{
	GENERATED_BODY()
public:
	virtual bool IsSupportedForNetworking() const override { return true; }
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;
	/** Grows on the owner every 100 steps. */
	UPROPERTY(Replicated) int32 Ammo = 0;
	UPROPERTY(Replicated) FString Weapon = TEXT("none");
};
