// Put this on a Chaos wheeled vehicle actor (next to UMusubiAuthorityComponent): the vehicle's snapshot — gear, engine
// RPM, each wheel's suspension offset / rotation / steering / radius / angular velocity — crosses the border with the
// rigid body, so the new owner continues the drive instead of re-settling the suspension and dropping the gear.
#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "MusubiHandoverState.h"
#include "MusubiVehicleHandover.generated.h"

UCLASS(ClassGroup = (Musubi), meta = (BlueprintSpawnableComponent))
class MUSUBIVEHICLES_API UMusubiVehicleHandover : public UActorComponent, public IMusubiHandoverState
{
	GENERATED_BODY()
public:
	virtual FName HandoverStateKey() const override { return TEXT("vehicle"); }
	virtual TSharedPtr<FJsonValue> CaptureHandoverState() override;
	virtual void RestoreHandoverState(const TSharedPtr<FJsonValue>& State) override;
};
