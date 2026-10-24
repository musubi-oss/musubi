#include "MusubiGameMode.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiJson.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "GameFramework/Pawn.h"
#include "GameFramework/PlayerController.h"
#include "Engine/NetConnection.h"
#include "GameFramework/PlayerState.h"
#include "Kismet/GameplayStatics.h"
#include "Dom/JsonObject.h"
#include "Dom/JsonValue.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiGameMode, Log, All);

AMusubiGameMode::AMusubiGameMode()
{
	PrimaryActorTick.bCanEverTick = true;
}

UMusubiSimulatorSubsystem* AMusubiGameMode::Sim() const
{
	const UGameInstance* GI = GetGameInstance();
	return GI ? GI->GetSubsystem<UMusubiSimulatorSubsystem>() : nullptr;
}

FString AMusubiGameMode::EntityOf(AController* Controller) const
{
	const FString* E = Controller ? Entities.Find(Controller) : nullptr;
	return E ? *E : FString();
}

APawn* AMusubiGameMode::OwnedPawnFor(const FString& Entity) const
{
	UMusubiSimulatorSubsystem* S = Sim();
	AActor* A = S ? S->FindEntityActor(Entity) : nullptr;
	APawn* P = Cast<APawn>(A);
	const UMusubiAuthorityComponent* Auth = P ? P->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
	return (Auth && Auth->IsOwned() && !Auth->IsInHandover()) ? P : nullptr;
}

APlayerController* AMusubiGameMode::Login(UPlayer* NewPlayer, ENetRole InRemoteRole, const FString& Portal, const FString& Options, const FUniqueNetIdRepl& UniqueId, FString& ErrorMessage)
{
	FString Opts = Options;
	if (UGameplayStatics::ParseOption(Options, TEXT("entity")).IsEmpty())
		if (const UNetConnection* Conn = Cast<UNetConnection>(NewPlayer))
		{
			const FString Entity = Conn->URL.GetOption(TEXT("entity="), TEXT(""));
			if (!Entity.IsEmpty()) Opts += FString::Printf(TEXT("?entity=%s"), *Entity);
		}
	return Super::Login(NewPlayer, InRemoteRole, Portal, Opts, UniqueId, ErrorMessage);
}

FString AMusubiGameMode::InitNewPlayer(APlayerController* NewPlayerController, const FUniqueNetIdRepl& UniqueId, const FString& Options, const FString& Portal)
{
	const FString Result = Super::InitNewPlayer(NewPlayerController, UniqueId, Options, Portal);
	const FString Entity = UGameplayStatics::ParseOption(Options, TEXT("entity"));
	if (!Entity.IsEmpty() && NewPlayerController)
	{
		Entities.Add(NewPlayerController, Entity);
		UE_LOG(LogMusubiGameMode, Log, TEXT("%s logs in for entity %s (%s here)"), *NewPlayerController->GetName(), *Entity,
			OwnedPawnFor(Entity) ? TEXT("owned") : TEXT("not owned yet"));
	}
	return Result;
}

APawn* AMusubiGameMode::SpawnDefaultPawnFor_Implementation(AController* NewPlayer, AActor* StartSpot)
{
	if (const FString* E = NewPlayer ? Entities.Find(NewPlayer) : nullptr)
		if (APawn* P = OwnedPawnFor(*E))
		{
			if (AController* Other = P->GetController(); Other && Other != NewPlayer) Other->UnPossess();
			return P;
		}
	return Super::SpawnDefaultPawnFor_Implementation(NewPlayer, StartSpot);
}

void AMusubiGameMode::PostLogin(APlayerController* NewPlayer)
{
	Super::PostLogin(NewPlayer);
	if (const FString* E = Entities.Find(NewPlayer)) Bind(NewPlayer, *E);
}

void AMusubiGameMode::Logout(AController* Exiting)
{
	if (APlayerController* PC = Cast<APlayerController>(Exiting))
		if (const FString* E = Entities.Find(Exiting))
			if (APawn* P = OwnedPawnFor(*E)) SaveState(PC, P);
	Entities.Remove(Exiting);
	Saved.Remove(Exiting);
	Super::Logout(Exiting);
}

void AMusubiGameMode::Bind(APlayerController* PC, const FString& Entity)
{
	APawn* P = OwnedPawnFor(Entity);
	if (!P) return;
	if (PC->GetPawn() != P)
	{
		APawn* Old = PC->GetPawn();
		if (AController* Other = P->GetController(); Other && Other != PC) Other->UnPossess();
		PC->Possess(P);
		UE_LOG(LogMusubiGameMode, Log, TEXT("%s possesses %s (entity %s)%s"), *PC->GetName(), *P->GetName(), *Entity, Old ? TEXT(", default pawn dropped") : TEXT(""));
		if (Old && Old != P && DefaultPawnClass && Old->IsA(DefaultPawnClass)) Old->Destroy();
		RestoreState(PC, P, Entity);
	}
	else if (!Saved.Contains(PC)) RestoreState(PC, P, Entity);
}

void AMusubiGameMode::RestoreState(APlayerController* PC, AActor* Actor, const FString& Entity)
{
	APlayerState* PS = PC ? PC->PlayerState : nullptr;
	UMusubiAuthorityComponent* Auth = Actor ? Actor->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
	if (!PS || !Auth) return;
	const FMusubiJson* V = Auth->FindComponent(PlayerComponent());
	const TSharedPtr<FJsonObject>* O = nullptr;
	if (V && V->Value.IsValid() && V->Value->TryGetObject(O) && O)
	{
		FString Name;
		double Score = 0;
		if ((*O)->TryGetStringField(TEXT("name"), Name) && !Name.IsEmpty()) PS->SetPlayerName(Name);
		if ((*O)->TryGetNumberField(TEXT("score"), Score)) PS->SetScore((float)Score);
		Saved.Add(PC, MusubiJson::Serialize(V->Value));
		UE_LOG(LogMusubiGameMode, Log, TEXT("%s restored from entity %s: name=%s score=%.0f"), *PC->GetName(), *Entity, *PS->GetPlayerName(), PS->GetScore());
	}
	else
	{
		// first owner ever: give the player a stable identity the next owners will inherit
		PS->SetPlayerName(FString::Printf(TEXT("player-%s"), *Entity.Right(4)));
		SaveState(PC, Actor);
		UE_LOG(LogMusubiGameMode, Log, TEXT("%s is new on entity %s: name=%s"), *PC->GetName(), *Entity, *PS->GetPlayerName());
	}
}

void AMusubiGameMode::SaveState(APlayerController* PC, AActor* Actor)
{
	APlayerState* PS = PC ? PC->PlayerState : nullptr;
	UMusubiAuthorityComponent* Auth = Actor ? Actor->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
	if (!PS || !Auth || !Auth->IsOwned()) return;
	TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
	O->SetStringField(TEXT("name"), PS->GetPlayerName());
	O->SetNumberField(TEXT("score"), PS->GetScore());
	FMusubiJson V;
	V.Value = MakeShared<FJsonValueObject>(O);
	const FString S = MusubiJson::Serialize(V.Value);
	if (const FString* Last = Saved.Find(PC); Last && *Last == S) return;
	Saved.Add(PC, S);
	Auth->SetComponent(PlayerComponent(), V);
}

void AMusubiGameMode::Tick(float DeltaSeconds)
{
	Super::Tick(DeltaSeconds);
	Accum += DeltaSeconds;
	if (Accum < 0.25f) return;
	Accum = 0.f;
	for (auto It = Entities.CreateIterator(); It; ++It)
	{
		APlayerController* PC = Cast<APlayerController>(It.Key().Get());
		if (!IsValid(PC)) { It.RemoveCurrent(); continue; }
		Bind(PC, It.Value());
		if (APawn* P = OwnedPawnFor(It.Value()); P && PC->GetPawn() == P) SaveState(PC, P);
	}
}
