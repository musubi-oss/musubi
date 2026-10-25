#include "MusubiNitlRagdoll.h"
#include "Components/SkeletalMeshComponent.h"
#include "Engine/SkeletalMesh.h"
#include "PhysicsEngine/BodyInstance.h"
#include "PhysicsEngine/PhysicsAsset.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiNitlLog.h"

AMusubiNitlRagdoll::AMusubiNitlRagdoll()
{
	PrimaryActorTick.bCanEverTick = true;
	Mesh = CreateDefaultSubobject<USkeletalMeshComponent>(TEXT("Mesh"));
	SetRootComponent(Mesh);
	Mesh->SetCollisionProfileName(TEXT("Ragdoll"));
	Mesh->SetEnableGravity(false);
	Authority = CreateDefaultSubobject<UMusubiAuthorityComponent>(TEXT("Authority"));
}

void AMusubiNitlRagdoll::BeginPlay()
{
	Super::BeginPlay();
	USkeletalMesh* SK = LoadObject<USkeletalMesh>(nullptr, TEXT("/Engine/Tutorial/SubEditors/TutorialAssets/Character/TutorialTPP.TutorialTPP"));
	UPhysicsAsset* PA = LoadObject<UPhysicsAsset>(nullptr, TEXT("/Engine/Tutorial/SubEditors/TutorialAssets/Character/TutorialTPP_PhysicsAsset.TutorialTPP_PhysicsAsset"));
	if (!SK || !PA)
	{
		UE_LOG(LogMusubiNitl, Error, TEXT("nitl: ragdoll assets missing (mesh=%d physics=%d)"), SK != nullptr, PA != nullptr);
		return;
	}
	Mesh->SetSkeletalMesh(SK);
	Mesh->SetPhysicsAsset(PA, true);
	Mesh->SetEnableGravity(false);
	// simulating from the first frame: the authority component remembers that as the body's nature (ghost copies go kinematic)
	Mesh->SetSimulatePhysics(true);
	Mesh->SetAllBodiesSimulatePhysics(true);
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: ragdoll %s bodies=%d"), *GetName(), Mesh->Bodies.Num());
}

void AMusubiNitlRagdoll::Tick(float Dt)
{
	Super::Tick(Dt);
	if (!Authority || !Mesh) return;
	int32 Simulating = 0;
	for (FBodyInstance* BI : Mesh->Bodies) if (BI && BI->IsInstanceSimulatingPhysics()) ++Simulating;
	const double Kick = Authority->GetComponentNumber(TEXT("nitl.kick"));
	if (Authority->IsOwned() && Kick > 0 && Simulating > 0)
	{
		Mesh->SetAllPhysicsLinearVelocity(FVector(Kick, 0, 0));
		Authority->SetComponentNumber(TEXT("nitl.kick"), 0);
		UE_LOG(LogMusubiNitl, Log, TEXT("nitl: ragdoll %s kicked +x %.0f (%d bodies)"), *Authority->GetEntityId(), Kick, Simulating);
	}
	const double Now = FPlatformTime::Seconds();
	if (Now < NextLog) return;
	NextLog = Now + 1.0;
	const FVector At = GetActorLocation();
	FVector Hand = At;
	if (const FBodyInstance* BI = Mesh->GetBodyInstance(TEXT("hand_r"))) Hand = BI->GetUnrealWorldTransform().GetLocation();
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: ragdoll %s owned=%d sim=%d/%d at %.0f,%.0f,%.0f hand_r=%.0f,%.0f,%.0f v=%.0f"), *Authority->GetEntityId(), (int)Authority->IsOwned(), Simulating, Mesh->Bodies.Num(),
		At.X, At.Y, At.Z, Hand.X, Hand.Y, Hand.Z, Mesh->GetPhysicsLinearVelocity().X);
}
