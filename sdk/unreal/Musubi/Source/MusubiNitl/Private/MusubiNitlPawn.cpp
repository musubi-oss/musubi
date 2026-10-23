#include "MusubiNitlPawn.h"
#include "MusubiAuthorityComponent.h"
#include "Components/SceneComponent.h"
#include "Net/UnrealNetwork.h"
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
	RootComponent = CreateDefaultSubobject<USceneComponent>(TEXT("Root"));
	Authority = CreateDefaultSubobject<UMusubiAuthorityComponent>(TEXT("Authority"));
	FParse::Value(FCommandLine::Get(), TEXT("MusubiNitlWalk="), Walk);
}

void AMusubiNitlPawn::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(AMusubiNitlPawn, Steps);
}

void AMusubiNitlPawn::Tick(float DeltaSeconds)
{
	Super::Tick(DeltaSeconds);
	if (!HasAuthority() || !Authority || !Authority->IsOwned() || Authority->IsInHandover()) return;
	// walk only once a client has joined the owner (login done, PlayerController spawned), so the border crossing happens in front of it
	const UNetDriver* ND = GetWorld() ? GetWorld()->GetNetDriver() : nullptr;
	bool bClientJoined = false;
	if (ND) for (const UNetConnection* C : ND->ClientConnections) if (C && C->PlayerController) { bClientJoined = true; break; }
	if (Walk > 0.f && bClientJoined) AddActorWorldOffset(FVector(Walk * DeltaSeconds, 0, 0));
	++Steps;
	const double Now = FPlatformTime::Seconds();
	if (Now - LastLog > 1.0)
	{
		LastLog = Now;
		const FVector L = GetActorLocation();
		UE_LOG(LogMusubiNitl, Log, TEXT("owner %s at %.0f,%.0f steps=%d cell=%s"), *Authority->GetEntityId(), L.X, L.Y, Steps, *Authority->GetCell());
	}
}

void AMusubiNitlPawn::OnRep_Steps()
{
	if (Steps % 30 == 0)
	{
		const FVector L = GetActorLocation();
		UE_LOG(LogMusubiNitl, Log, TEXT("client sees %s (%s) at %.0f,%.0f steps=%d owned=%d"), *GetName(), Authority ? *Authority->NetEntityId : TEXT("?"), L.X, L.Y, Steps, Authority ? (int)Authority->bNetOwned : -1);
	}
}
