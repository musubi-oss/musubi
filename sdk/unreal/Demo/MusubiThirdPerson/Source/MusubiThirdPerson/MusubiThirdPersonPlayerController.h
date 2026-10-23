#pragma once

#include "CoreMinimal.h"
#include "GameFramework/PlayerController.h"
#include "MusubiThirdPersonPlayerController.generated.h"

class UInputMappingContext;

UCLASS(abstract)
class MUSUBITHIRDPERSON_API AMusubiThirdPersonPlayerController : public APlayerController
{
	GENERATED_BODY()
protected:
	UPROPERTY(EditAnywhere, Category = "Input|Input Mappings") TArray<TObjectPtr<UInputMappingContext>> DefaultMappingContexts;
	UPROPERTY(EditAnywhere, Category = "Input|Input Mappings") TArray<TObjectPtr<UInputMappingContext>> MobileExcludedMappingContexts;
	/** Kept for the template's Blueprint; touch controls are not part of the demo. */
	UPROPERTY(EditAnywhere, Category = "Input|Touch Controls") TSubclassOf<class UUserWidget> MobileControlsWidgetClass;
	UPROPERTY(EditAnywhere, Config, Category = "Input|Touch Controls") bool bForceTouchControls = false;
	virtual void SetupInputComponent() override;
};
