#pragma once

#include "CoreMinimal.h"
#include "Commandlets/Commandlet.h"
#include "MusubiDemoContentCommandlet.generated.h"

/**
 * `UnrealEditor-Cmd MusubiThirdPerson.uproject -run=MusubiDemoContent`: writes the demo's Blueprint logic into
 * BP_ThirdPersonCharacter's event graph (the same nodes an integrator would place by hand), compiles and saves it.
 * Re-runnable: nodes it placed before are removed first. See docs/gdk-unreal.md §9.11.
 */
UCLASS()
class UMusubiDemoContentCommandlet : public UCommandlet
{
	GENERATED_BODY()
public:
	UMusubiDemoContentCommandlet();
	virtual int32 Main(const FString& Params) override;
};
