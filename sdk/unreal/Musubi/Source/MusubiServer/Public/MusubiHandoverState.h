// Extra per-actor state that travels with a handover (and a node-initiated handover_started) inside `musubi.phys.ext`:
// a component implementing this contributes one JSON value under its key on the old owner and gets it back on the new
// one, after the root body and the per-body (ragdoll) state were restored. MusubiVehicles uses it for the Chaos vehicle
// snapshot; a game adds its own (fuel, heat, anything the engine simulates outside the rigid bodies).
#pragma once
#include "CoreMinimal.h"
#include "Dom/JsonValue.h"
#include "UObject/Interface.h"
#include "MusubiHandoverState.generated.h"

UINTERFACE(MinimalAPI)
class UMusubiHandoverState : public UInterface
{
	GENERATED_BODY()
};

class MUSUBISERVER_API IMusubiHandoverState
{
	GENERATED_BODY()
public:
	/** Key under `musubi.phys.ext`; one per component (two components with the same key overwrite each other). */
	virtual FName HandoverStateKey() const = 0;
	/** State at the moment the old owner freezes the body; null = nothing to send. */
	virtual TSharedPtr<FJsonValue> CaptureHandoverState() = 0;
	/** Called on the new owner right after the rigid bodies resumed. */
	virtual void RestoreHandoverState(const TSharedPtr<FJsonValue>& State) = 0;
};
