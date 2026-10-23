#include "MusubiThirdPersonCharacter.h"
#include "MusubiThirdPersonInventory.h"
#include "MusubiThirdPerson.h"
#include "MusubiAuthorityComponent.h"
#include "Camera/CameraComponent.h"
#include "Components/CapsuleComponent.h"
#include "GameFramework/CharacterMovementComponent.h"
#include "GameFramework/SpringArmComponent.h"
#include "GameFramework/Controller.h"
#include "EnhancedInputComponent.h"
#include "InputActionValue.h"

AMusubiThirdPersonCharacter::AMusubiThirdPersonCharacter()
{
	GetCapsuleComponent()->InitCapsuleSize(42.f, 96.0f);
	bUseControllerRotationPitch = false;
	bUseControllerRotationYaw = false;
	bUseControllerRotationRoll = false;

	UCharacterMovementComponent* Move = GetCharacterMovement();
	Move->bOrientRotationToMovement = true;
	Move->RotationRate = FRotator(0.0f, 500.0f, 0.0f);
	Move->JumpZVelocity = 500.f;
	Move->AirControl = 0.35f;
	Move->MaxWalkSpeed = 500.f;
	Move->MinAnalogWalkSpeed = 20.f;
	Move->BrakingDecelerationWalking = 2000.f;
	Move->BrakingDecelerationFalling = 1500.0f;

	CameraBoom = CreateDefaultSubobject<USpringArmComponent>(TEXT("CameraBoom"));
	CameraBoom->SetupAttachment(RootComponent);
	CameraBoom->TargetArmLength = 400.0f;
	CameraBoom->bUsePawnControlRotation = true;
	FollowCamera = CreateDefaultSubobject<UCameraComponent>(TEXT("FollowCamera"));
	FollowCamera->SetupAttachment(CameraBoom, USpringArmComponent::SocketName);
	FollowCamera->bUsePawnControlRotation = false;

	bReplicates = true;
	SetReplicatingMovement(true);
	Authority = CreateDefaultSubobject<UMusubiAuthorityComponent>(TEXT("Authority"));
	Inventory = CreateDefaultSubobject<UMusubiThirdPersonInventory>(TEXT("Inventory"));
}

bool AMusubiThirdPersonCharacter::IsEntityOwned() const { return Authority && Authority->IsOwned(); }
FString AMusubiThirdPersonCharacter::GetEntityId() const { return Authority ? Authority->GetEntityUuid().ToString() : FString(); }

void AMusubiThirdPersonCharacter::SetupPlayerInputComponent(UInputComponent* PlayerInputComponent)
{
	UEnhancedInputComponent* EIC = Cast<UEnhancedInputComponent>(PlayerInputComponent);
	if (!EIC) { UE_LOG(LogMusubiThirdPerson, Error, TEXT("'%s': no Enhanced Input component"), *GetNameSafe(this)); return; }
	EIC->BindAction(JumpAction, ETriggerEvent::Started, this, &ACharacter::Jump);
	EIC->BindAction(JumpAction, ETriggerEvent::Completed, this, &ACharacter::StopJumping);
	EIC->BindAction(MoveAction, ETriggerEvent::Triggered, this, &AMusubiThirdPersonCharacter::Move);
	EIC->BindAction(MouseLookAction, ETriggerEvent::Triggered, this, &AMusubiThirdPersonCharacter::Look);
	EIC->BindAction(LookAction, ETriggerEvent::Triggered, this, &AMusubiThirdPersonCharacter::Look);
}

void AMusubiThirdPersonCharacter::Move(const FInputActionValue& Value) { const FVector2D V = Value.Get<FVector2D>(); DoMove(V.X, V.Y); }
void AMusubiThirdPersonCharacter::Look(const FInputActionValue& Value) { const FVector2D V = Value.Get<FVector2D>(); DoLook(V.X, V.Y); }

void AMusubiThirdPersonCharacter::DoMove(float Right, float Forward)
{
	if (!GetController()) return;
	const FRotator YawRotation(0, GetController()->GetControlRotation().Yaw, 0);
	AddMovementInput(FRotationMatrix(YawRotation).GetUnitAxis(EAxis::X), Forward);
	AddMovementInput(FRotationMatrix(YawRotation).GetUnitAxis(EAxis::Y), Right);
}

void AMusubiThirdPersonCharacter::DoLook(float Yaw, float Pitch)
{
	if (!GetController()) return;
	AddControllerYawInput(Yaw);
	AddControllerPitchInput(Pitch);
}

void AMusubiThirdPersonCharacter::DoJumpStart() { Jump(); }
void AMusubiThirdPersonCharacter::DoJumpEnd() { StopJumping(); }
