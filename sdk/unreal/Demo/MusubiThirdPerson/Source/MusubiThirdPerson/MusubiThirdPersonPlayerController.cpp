#include "MusubiThirdPersonPlayerController.h"
#include "EnhancedInputSubsystems.h"
#include "Engine/LocalPlayer.h"
#include "InputMappingContext.h"

void AMusubiThirdPersonPlayerController::SetupInputComponent()
{
	Super::SetupInputComponent();
	if (!IsLocalPlayerController()) return;
	UEnhancedInputLocalPlayerSubsystem* S = ULocalPlayer::GetSubsystem<UEnhancedInputLocalPlayerSubsystem>(GetLocalPlayer());
	if (!S) return;
	for (UInputMappingContext* C : DefaultMappingContexts) S->AddMappingContext(C, 0);
	for (UInputMappingContext* C : MobileExcludedMappingContexts) S->AddMappingContext(C, 0);
}
