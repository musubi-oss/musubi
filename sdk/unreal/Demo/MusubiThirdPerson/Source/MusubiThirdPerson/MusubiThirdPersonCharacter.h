// Epic's Third Person template character (camera boom, follow camera, Enhanced Input) as a Musubi entity: the
// UMusubiAuthorityComponent makes it owned/ghost on a dedicated server, the inventory rides the entity through handovers.
#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Character.h"
#include "MusubiThirdPersonCharacter.generated.h"

class USpringArmComponent;
class UCameraComponent;
class UInputAction;
class UMusubiAuthorityComponent;
class UMusubiThirdPersonInventory;
struct FInputActionValue;

UCLASS(abstract)
class MUSUBITHIRDPERSON_API AMusubiThirdPersonCharacter : public ACharacter
{
	GENERATED_BODY()

	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category = "Components", meta = (AllowPrivateAccess = "true"))
	TObjectPtr<USpringArmComponent> CameraBoom;
	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category = "Components", meta = (AllowPrivateAccess = "true"))
	TObjectPtr<UCameraComponent> FollowCamera;

protected:
	UPROPERTY(EditAnywhere, Category = "Input") TObjectPtr<UInputAction> JumpAction;
	UPROPERTY(EditAnywhere, Category = "Input") TObjectPtr<UInputAction> MoveAction;
	UPROPERTY(EditAnywhere, Category = "Input") TObjectPtr<UInputAction> LookAction;
	UPROPERTY(EditAnywhere, Category = "Input") TObjectPtr<UInputAction> MouseLookAction;

public:
	AMusubiThirdPersonCharacter();

	/** Musubi authority: entity id, owned/ghost, handover state (docs §6). */
	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category = "Musubi") TObjectPtr<UMusubiAuthorityComponent> Authority;
	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category = "Musubi") TObjectPtr<UMusubiThirdPersonInventory> Inventory;

	/** True on the server that owns this character's entity (the one simulating it); false on ghosts and clients. */
	UFUNCTION(BlueprintPure, Category = "Musubi") bool IsEntityOwned() const;
	/** The Musubi entity id as text (empty on a client before the actor is identified). */
	UFUNCTION(BlueprintPure, Category = "Musubi") FString GetEntityId() const;

protected:
	virtual void SetupPlayerInputComponent(UInputComponent* PlayerInputComponent) override;
	void Move(const FInputActionValue& Value);
	void Look(const FInputActionValue& Value);

public:
	UFUNCTION(BlueprintCallable, Category = "Input") virtual void DoMove(float Right, float Forward);
	UFUNCTION(BlueprintCallable, Category = "Input") virtual void DoLook(float Yaw, float Pitch);
	UFUNCTION(BlueprintCallable, Category = "Input") virtual void DoJumpStart();
	UFUNCTION(BlueprintCallable, Category = "Input") virtual void DoJumpEnd();

	FORCEINLINE USpringArmComponent* GetCameraBoom() const { return CameraBoom; }
	FORCEINLINE UCameraComponent* GetFollowCamera() const { return FollowCamera; }
};
