#include "MusubiNitlSubsystem.h"
#include "Engine/NetworkDelegates.h"
#include "MusubiNitlPawn.h"
#include "MusubiNitlLoadout.h"
#include "MusubiNitlInventoryComponent.h"
#include "Net/Serialization/FastArraySerializer.h"
#include "GameFramework/Pawn.h"
#include "GameFramework/Character.h"
#include "GameFramework/CharacterMovementComponent.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiSubsystem.h"
#include "MusubiJson.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "EngineUtils.h"
#include "Kismet/GameplayStatics.h"
#include "GameFramework/PlayerController.h"
#include "GameFramework/PlayerState.h"
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
		IdentifiedHandle = UMusubiAuthorityComponent::OnClientEntityIdentified.AddLambda([this](AActor* A, const FString& Id)
		{
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: identified %s as %s"), A ? *A->GetName() : TEXT("?"), *Id);
			if (!Handle.IsSet()) Handle = UMusubiEntityRegistry::HandleOf(A);
		});
		C->AuthenticateDevice(FGuid::NewGuid().ToString(EGuidFormats::Digits));
	}
	// -MusubiNitlEncrypt: a fixed AES-256-GCM key on both ends, the way a game's key provider would answer the engine's
	// encryption handshake (client sends ?EncryptionToken=, the owner — and every new owner after a rehome — acks it)
	if (FParse::Param(FCommandLine::Get(), TEXT("MusubiNitlEncrypt")))
	{
		static const auto TestKey = []
		{
			FEncryptionData D;
			D.Identifier = TEXT("nitl");
			for (int32 i = 0; i < 32; ++i) D.Key.Add((uint8)(0x5a ^ i));
			return D;
		};
		FNetDelegates::OnReceivedNetworkEncryptionToken.BindLambda([](const FString& Token, const FOnEncryptionKeyResponse& Delegate)
		{
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: encryption token '%s' -> keying the connection"), *Token);
			FEncryptionKeyResponse R(EEncryptionResponse::Success, TEXT(""));
			R.EncryptionData = TestKey();
			Delegate.ExecuteIfBound(R);
		});
		FNetDelegates::OnReceivedNetworkEncryptionAck.BindLambda([](const FOnEncryptionKeyResponse& Delegate)
		{
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: encryption ack -> keying the connection"));
			FEncryptionKeyResponse R(EEncryptionResponse::Success, TEXT(""));
			R.EncryptionData = TestKey();
			Delegate.ExecuteIfBound(R);
		});
	}
	Ticker = FTSTicker::GetCoreTicker().AddTicker(FTickerDelegate::CreateUObject(this, &UMusubiNitlSubsystem::Report), 1.f);
	if (Mode == TEXT("client")) TickHandle = FWorldDelegates::OnWorldTickStart.AddUObject(this, &UMusubiNitlSubsystem::OnWorldTick);
}

void UMusubiNitlSubsystem::OnWorldTick(UWorld* World, ELevelTick, float)
{
	// the player walks +X: input on the possessed pawn, CharacterMovement predicts locally and sends ServerMove to whoever owns the entity
	if (!World || !GetGameInstance() || World != GetGameInstance()->GetWorld()) return;
	APlayerController* PC = UGameplayStatics::GetPlayerController(World, 0);
	APawn* P = PC ? PC->GetPawn() : nullptr;
	static const float Walk = [] { float W = 60.f; FParse::Value(FCommandLine::Get(), TEXT("MusubiNitlWalk="), W); return W; }();
	if (Walk <= 0.f) return; // -MusubiNitlWalk=0: a spectator whose pawn stays put
	if (P && P->FindComponentByClass<UMusubiAuthorityComponent>()) P->AddMovementInput(FVector::XAxisVector, 1.f);
}

void UMusubiNitlSubsystem::Deinitialize()
{
	FTSTicker::GetCoreTicker().RemoveTicker(Ticker);
	FWorldDelegates::OnWorldTickStart.Remove(TickHandle);
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
	FString X, Y, Z;
	if (!At.Split(TEXT(","), &X, &Y)) return;
	if (Y.Split(TEXT(","), &Y, &Z)) {} // -MusubiNitlSpawn=x,y[,z]: z = the simulator's height component (a map with a floor)
	FMusubiJson Components;
	TSharedRef<FJsonObject> Obj = MakeShared<FJsonObject>();
	if (!Z.IsEmpty()) Obj->SetNumberField(TEXT("z"), FCString::Atod(*Z));
	Components.Value = MakeShared<FJsonValueObject>(Obj);
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
	// -MusubiNitlJoin: no entity on the URL, the driver asks the node for this account's avatar (world_join)
	const bool bJoin = FParse::Param(FCommandLine::Get(), TEXT("MusubiNitlJoin"));
	if (W.IsEmpty() || (E.IsEmpty() && !bJoin) || !World || !GEngine) { UE_LOG(LogMusubiNitl, Error, TEXT("nitl: need -MusubiWorld= and -MusubiEntity= (or -MusubiNitlJoin)")); return; }
	bTravelled = true;
	FString URL = bJoin ? FString::Printf(TEXT("musubi://%s:0"), *W) : FString::Printf(TEXT("musubi://%s:0?entity=%s"), *W, *E);
	if (FParse::Param(FCommandLine::Get(), TEXT("MusubiNitlEncrypt"))) URL += TEXT("?EncryptionToken=nitl"); // FURL options are all ?-separated
	UE_LOG(LogMusubiNitl, Log, TEXT("nitl: travelling to %s"), *URL);
	GEngine->SetClientTravel(World, *URL, TRAVEL_Absolute);
}

void UMusubiNitlSubsystem::OnSocketClosed(int32 Code, const FString& Reason)
{
	UE_LOG(LogMusubiNitl, Warning, TEXT("nitl: socket closed %d %s"), Code, *Reason);
}

// The report is pawn-agnostic so the same checks run on the NITL pawn and on a game's own character (the Third Person
// demo): any pawn with an authority component counts; Steps/PushSteps come from the NITL pawn only, the inventory is
// read through reflection from a component named "Inventory" (int `Gold` + the first FastArray's item count).
static void ReadInventory(const APawn* P, int32& OutItems, int32& OutGold)
{
	OutItems = -1; OutGold = -1;
	const UActorComponent* Inv = nullptr;
	for (const UActorComponent* C : P->GetComponents()) if (C && C->GetFName() == TEXT("Inventory")) { Inv = C; break; }
	if (!Inv) return;
	if (const FIntProperty* G = CastField<FIntProperty>(Inv->GetClass()->FindPropertyByName(TEXT("Gold")))) OutGold = G->GetPropertyValue_InContainer(Inv);
	for (TFieldIterator<FStructProperty> It(Inv->GetClass()); It; ++It)
	{
		if (!It->Struct || !It->Struct->IsChildOf(FFastArraySerializer::StaticStruct())) continue;
		TFieldIterator<FArrayProperty> A(It->Struct);
		if (!A) continue;
		FScriptArrayHelper H(*A, A->ContainerPtrToValuePtr<void>(It->ContainerPtrToValuePtr<void>(Inv)));
		OutItems = H.Num();
		return;
	}
}

// The first registered subobject with an int `Ammo` (the NITL pawn's Loadout), -1 when the pawn has none. By property,
// not by name: a dynamically replicated subobject is created on the client under a generated name.
static int32 ReadAmmo(const UMusubiAuthorityComponent* Authority)
{
	for (const TWeakObjectPtr<UObject>& W : Authority->MusubiSubObjects())
		if (const UObject* L = W.Get())
			if (const FIntProperty* P = CastField<FIntProperty>(L->GetClass()->FindPropertyByName(TEXT("Ammo")))) return P->GetPropertyValue_InContainer(L);
	return -1;
}

bool UMusubiNitlSubsystem::Report(float)
{
	UWorld* World = GetGameInstance() ? GetGameInstance()->GetWorld() : nullptr;
	if (!World) return true;
	int32 N = 0;
	for (TActorIterator<APawn> It(World); It; ++It)
	{
		const UMusubiAuthorityComponent* Authority = It->FindComponentByClass<UMusubiAuthorityComponent>();
		if (!Authority) continue;
		const AMusubiNitlPawn* Nitl = Cast<AMusubiNitlPawn>(*It);
		int32 Items, Gold; ReadInventory(*It, Items, Gold);
		const FVector L = It->GetActorLocation();
		const APlayerController* LocalPC = Mode == TEXT("client") ? UGameplayStatics::GetPlayerController(World, 0) : nullptr;
		const APlayerState* PS = LocalPC ? LocalPC->PlayerState.Get() : It->GetPlayerState();
		const ACharacter* Ch = Cast<ACharacter>(*It);
		const int32 MoveMode = Ch && Ch->GetCharacterMovement() ? (int32)Ch->GetCharacterMovement()->MovementMode.GetValue() : -1;
		UE_LOG(LogMusubiNitl, Log, TEXT("nitl: %s pawn %s id=%s owned=%d at %.0f,%.0f z=%.0f mode=%d steps=%d push=%d pushid=%016llx ctrl=%s possessed=%d name=%s inv=%d gold=%d ammo=%d local=%d sub=%s ps=%s"), *Mode, *It->GetName(),
			*Authority->NetEntityId, (int)Authority->bNetOwned, L.X, L.Y, L.Z, MoveMode, Nitl ? Nitl->Steps : -1, Nitl ? Nitl->PushSteps : -1, It->GetNetPushIdDynamic(),
			It->GetController() ? *It->GetController()->GetName() : TEXT("none"), LocalPC ? (int)(LocalPC->GetPawn() == *It) : (int)(It->GetController() != nullptr),
			PS ? *PS->GetPlayerName() : TEXT("-"), Items, Gold, ReadAmmo(Authority), Nitl ? Nitl->LocalTicks : -1,
			Nitl && Nitl->Loadout ? *Nitl->Loadout->GetName() : TEXT("-"), PS ? *PS->GetName() : TEXT("-"));
		++N;
	}
	if (Mode == TEXT("client"))
	{
		UE_LOG(LogMusubiNitl, Log, TEXT("nitl: client pawns=%d netdriver=%s"), N, World->GetNetDriver() ? *World->GetNetDriver()->GetDescription() : TEXT("none"));
		if (Handle.IsSet())
		{
			AActor* A = Handle.Get(World);
			const APlayerController* LocalPC = UGameplayStatics::GetPlayerController(World, 0);
			const UMusubiEntityRegistry* R = UMusubiEntityRegistry::Get(World);
			UE_LOG(LogMusubiNitl, Log, TEXT("nitl: handle %s -> %s changes=%d live=%d"), *Handle.EntityId, A ? *A->GetName() : TEXT("null"),
				R ? R->ChangeCount() : -1, (int)(A && LocalPC && LocalPC->GetPawn() == A));
		}
	}
	return true;
}
