#include "MusubiNitlPawn.h"
#include "MusubiAuthorityComponent.h"
#include "GameFramework/CharacterMovementComponent.h"
#include "GameFramework/Controller.h"
#include "Net/UnrealNetwork.h"
#include "Net/Core/PushModel/PushModel.h"
#include "Engine/NetDriver.h"
#include "Engine/NetConnection.h"
#include "Engine/World.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"

#include "MusubiNitlLog.h"

AMusubiNitlPawn::AMusubiNitlPawn()
{
	PrimaryActorTick.bCanEverTick = true;
	bReplicates = true;
	SetReplicatingMovement(true);
	Authority = CreateDefaultSubobject<UMusubiAuthorityComponent>(TEXT("Authority"));
	// the Entry map has no floor: fly, at the speed the run asks for (both sides parse it so the client's prediction agrees)
	float Walk = 60.f;
	FParse::Value(FCommandLine::Get(), TEXT("MusubiNitlWalk="), Walk);
	if (UCharacterMovementComponent* M = GetCharacterMovement())
	{
		M->DefaultLandMovementMode = MOVE_Flying;
		M->GravityScale = 0.f;
		M->MaxFlySpeed = Walk;
		M->BrakingDecelerationFlying = 4096.f;
	}
}

void AMusubiNitlPawn::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(AMusubiNitlPawn, Steps);
	FDoRepLifetimeParams Push;
	Push.bIsPushBased = true;
	DOREPLIFETIME_WITH_PARAMS_FAST(AMusubiNitlPawn, PushSteps, Push);
}

void AMusubiNitlPawn::Tick(float DeltaSeconds)
{
	Super::Tick(DeltaSeconds);
	if (!HasAuthority() || !Authority || !Authority->IsOwned() || Authority->IsInHandover()) return;
	// movement comes from the possessing client's input (CharacterMovement ServerMove over the relay); the owner only counts
	++Steps;
	PushSteps = Steps;
	MARK_PROPERTY_DIRTY_FROM_NAME(AMusubiNitlPawn, PushSteps, this);
	const double Now = FPlatformTime::Seconds();
	if (Now - LastLog > 1.0)
	{
		LastLog = Now;
		const FVector L = GetActorLocation();
		UE_LOG(LogMusubiNitl, Log, TEXT("owner %s at %.0f,%.0f steps=%d cell=%s ctrl=%s"), *Authority->GetEntityId(), L.X, L.Y, Steps, *Authority->GetCell(), GetController() ? *GetController()->GetName() : TEXT("none"));
	}
}

void AMusubiNitlPawn::PossessedBy(AController* NewController)
{
	Super::PossessedBy(NewController);
	// no floor on the Entry map: a possessed character would be Falling with air control only; fly (replicated to the client)
	if (UCharacterMovementComponent* M = GetCharacterMovement()) M->SetMovementMode(MOVE_Flying);
	UE_LOG(LogMusubiNitl, Log, TEXT("%s possessed by %s, movement mode=%d"), *GetName(), NewController ? *NewController->GetName() : TEXT("none"), GetCharacterMovement() ? (int)GetCharacterMovement()->MovementMode.GetValue() : -1);
}

void AMusubiNitlPawn::EndPlay(const EEndPlayReason::Type Reason)
{
	UE_LOG(LogMusubiNitl, Log, TEXT("%s EndPlay reason=%d steps=%d"), *GetName(), (int)Reason, Steps);
	Super::EndPlay(Reason);
}

void AMusubiNitlPawn::OnRep_Steps()
{
	if (Steps % 30 == 0)
	{
		const FVector L = GetActorLocation();
		UE_LOG(LogMusubiNitl, Log, TEXT("client sees %s (%s) at %.0f,%.0f steps=%d push=%d owned=%d"), *GetName(), Authority ? *Authority->NetEntityId : TEXT("?"), L.X, L.Y, Steps, PushSteps, Authority ? (int)Authority->bNetOwned : -1);
	}
}
