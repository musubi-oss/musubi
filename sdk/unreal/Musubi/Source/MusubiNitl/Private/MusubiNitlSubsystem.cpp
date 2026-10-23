#include "MusubiNitlSubsystem.h"
#include "MusubiNitlPawn.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiSubsystem.h"
#include "MusubiJson.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "EngineUtils.h"
#include "Engine/NetDriver.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"
#include "Subsystems/SubsystemCollection.h"

#include "MusubiNitlLog.h"

bool UMusubiNitlSubsystem::ShouldCreateSubsystem(UObject* Outer) const
{
	FString M;
	return Super::ShouldCreateSubsystem(Outer) && FParse::Value(FCommandLine::Get(), TEXT("MusubiNitl="), M) && !M.IsEmpty();
}

void UMusubiNitlSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);
	FParse::Value(FCommandLine::Get(), TEXT("MusubiNitl="), Mode);
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: mode=%s"), *Mode);
	if (Mode == TEXT("server"))
	{
		UMusubiSimulatorSubsystem* S = Collection.InitializeDependency<UMusubiSimulatorSubsystem>();
		if (!S) { UE_LOG(LogMusubiNitl, Error, TEXT("nitl: no simulator subsystem")); return; }
		Sim = S;
		S->OnHandover.AddDynamic(this, &UMusubiNitlSubsystem::OnHandover);
		S->OnCellChanged.AddDynamic(this, &UMusubiNitlSubsystem::OnCellChanged);
	}
	else if (Mode == TEXT("client"))
	{
		UMusubiSubsystem* C = Collection.InitializeDependency<UMusubiSubsystem>();
		if (!C) { UE_LOG(LogMusubiNitl, Error, TEXT("nitl: no client subsystem")); return; }
		Client = C;
		C->OnSession.AddDynamic(this, &UMusubiNitlSubsystem::OnSession);
		C->OnSocketConnected.AddDynamic(this, &UMusubiNitlSubsystem::OnSocketConnected);
		C->OnSocketClosed.AddDynamic(this, &UMusubiNitlSubsystem::OnSocketClosed);
		IdentifiedHandle = UMusubiAuthorityComponent::OnClientEntityIdentified.AddLambda([](AActor* A, const FString& Id)
		{
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: identified %s as %s"), A ? *A->GetName() : TEXT("?"), *Id);
		});
		C->AuthenticateDevice(FGuid::NewGuid().ToString(EGuidFormats::Digits));
	}
	Ticker = FTSTicker::GetCoreTicker().AddTicker(FTickerDelegate::CreateUObject(this, &UMusubiNitlSubsystem::Report), 1.f);
}

void UMusubiNitlSubsystem::Deinitialize()
{
	FTSTicker::GetCoreTicker().RemoveTicker(Ticker);
	UMusubiAuthorityComponent::OnClientEntityIdentified.Remove(IdentifiedHandle);
	Super::Deinitialize();
}

void UMusubiNitlSubsystem::OnCellChanged(const FString& Cell, bool bAssigned)
{
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: cell %s assigned=%d"), *Cell, (int)bAssigned);
	if (bAssigned) OnSimStarted();
}

void UMusubiNitlSubsystem::OnSimStarted()
{
	FString At;
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: sim started, spawn arg present=%d"), (int)FParse::Value(FCommandLine::Get(), TEXT("MusubiNitlSpawn="), At, false));
	if (!FParse::Value(FCommandLine::Get(), TEXT("MusubiNitlSpawn="), At, false) || !Sim.IsValid() || bSpawned) return;
	bSpawned = true;
	FString X, Y;
	if (!At.Split(TEXT(","), &X, &Y)) return;
	FMusubiJson Components;
	Components.Value = MakeShared<FJsonValueObject>(MakeShared<FJsonObject>());
	Sim->Spawn(FCString::Atod(*X), FCString::Atod(*Y), Components, false, false, false, TOptional<uint64>(), [](bool bOk, const TSharedPtr<FJsonObject>& R)
	{
		const TSharedPtr<FJsonObject>* E = nullptr;
		FString Id;
		if (bOk && R.IsValid() && R->TryGetObjectField(TEXT("entity"), E) && E) (*E)->TryGetStringField(TEXT("id"), Id);
		if (Id.IsEmpty())
		{
			const FString Raw = R.IsValid() ? MusubiJson::Serialize(MakeShared<FJsonValueObject>(R)) : FString(TEXT("no reply"));
			UE_LOG(LogMusubiNitl, Error, TEXT("nitl: spawn failed: %s"), *Raw);
		}
		else
		{
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: spawned %s"), *Id);
		}
	});
}

void UMusubiNitlSubsystem::OnHandover(const FString& EntityId, const FString& Phase, const FString& Detail)
{
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: handover %s %s %s"), *EntityId, *Phase, *Detail);
}

void UMusubiNitlSubsystem::OnSession(bool bOk, const FMusubiSession& Session)
{
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: session ok=%d"), (int)bOk);
	if (bOk && Client.IsValid()) Client->ConnectSocket();
}

void UMusubiNitlSubsystem::OnSocketConnected()
{
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: socket connected"));
	if (bTravelled) return;
	FString W, E;
	FParse::Value(FCommandLine::Get(), TEXT("MusubiWorld="), W);
	FParse::Value(FCommandLine::Get(), TEXT("MusubiEntity="), E);
	UWorld* World = GetGameInstance() ? GetGameInstance()->GetWorld() : nullptr;
	if (W.IsEmpty() || E.IsEmpty() || !World || !GEngine) { UE_LOG(LogMusubiNitl, Error, TEXT("nitl: need -MusubiWorld= and -MusubiEntity=")); return; }
	bTravelled = true;
	const FString URL = FString::Printf(TEXT("musubi://%s:0?entity=%s"), *W, *E);
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: travelling to %s"), *URL);
	GEngine->SetClientTravel(World, *URL, TRAVEL_Absolute);
}

void UMusubiNitlSubsystem::OnSocketClosed(int32 Code, const FString& Reason)
{
	UE_LOG(LogMusubiNitl, Warning, TEXT("nitl: socket closed %d %s"), Code, *Reason);
}

bool UMusubiNitlSubsystem::Report(float)
{
	UWorld* World = GetGameInstance() ? GetGameInstance()->GetWorld() : nullptr;
	if (!World) return true;
	int32 N = 0;
	for (TActorIterator<AMusubiNitlPawn> It(World); It; ++It)
	{
		const FVector L = It->GetActorLocation();
		UE_LOG(LogMusubiNitl, Log, TEXT("nitl: %s pawn %s id=%s owned=%d at %.0f,%.0f steps=%d push=%d pushid=%016llx"), *Mode, *It->GetName(),
			It->Authority ? *It->Authority->NetEntityId : TEXT("?"), It->Authority ? (int)It->Authority->bNetOwned : -1, L.X, L.Y, It->Steps, It->PushSteps, It->GetNetPushIdDynamic());
		++N;
	}
	if (Mode == TEXT("client"))
		UE_LOG(LogMusubiNitl, Log, TEXT("nitl: client pawns=%d netdriver=%s"), N, World->GetNetDriver() ? *World->GetNetDriver()->GetDescription() : TEXT("none"));
	return true;
}
