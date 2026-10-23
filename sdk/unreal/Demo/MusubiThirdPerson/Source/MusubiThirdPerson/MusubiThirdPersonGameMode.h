#pragma once

#include "CoreMinimal.h"
#include "MusubiGameMode.h"
#include "MusubiThirdPersonGameMode.generated.h"

/** The template game mode on top of AMusubiGameMode: possession follows the player's entity across servers (docs §9.7). */
UCLASS(abstract)
class MUSUBITHIRDPERSON_API AMusubiThirdPersonGameMode : public AMusubiGameMode
{
	GENERATED_BODY()
public:
	AMusubiThirdPersonGameMode();
};
