#pragma once
#include "CoreMinimal.h"
#include "GameFramework/GameModeBase.h"
#include "MusubiGameMode.generated.h"

/**
 * Game mode for Musubi dedicated servers. A player logs in with `?entity=<uuid>` (the URL the UMusubiNetDriver client
 * connects with) and is bound to that entity: if its actor is owned on this server and is a pawn, the player possesses
 * it instead of getting a default pawn; while the entity is a ghost here (or still in handover) the player gets the
 * default pawn and is moved onto the entity the moment it becomes owned. After a handover the client is rehomed to the
 * new owner and logs in there with the same URL, so possession follows the entity across servers. PlayerState name and
 * score ride the entity's `musubi.player` component, written by the owner, so the next owner restores them.
 */
UCLASS()
class MUSUBISERVER_API AMusubiGameMode : public AGameModeBase
{
	GENERATED_BODY()
public:
	AMusubiGameMode();
	/** A player who joined through `world_join` (musubi://<world>:0, no ?entity=) has no entity in its login options; the relay connection carries it (`UNetConnection::URL`), so it is added to the options here before the regular login. */
	virtual APlayerController* Login(UPlayer* NewPlayer, ENetRole InRemoteRole, const FString& Portal, const FString& Options, const FUniqueNetIdRepl& UniqueId, FString& ErrorMessage) override;
	virtual FString InitNewPlayer(APlayerController* NewPlayerController, const FUniqueNetIdRepl& UniqueId, const FString& Options, const FString& Portal = TEXT("")) override;
	virtual APawn* SpawnDefaultPawnFor_Implementation(AController* NewPlayer, AActor* StartSpot) override;
	virtual void PostLogin(APlayerController* NewPlayer) override;
	virtual void Logout(AController* Exiting) override;
	virtual void Tick(float DeltaSeconds) override;

	static const TCHAR* PlayerComponent() { return TEXT("musubi.player"); }
	/** The entity the controller logged in for, empty if none. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Player") FString EntityOf(AController* Controller) const;

protected:
	class UMusubiSimulatorSubsystem* Sim() const;
	/** The entity's actor if it is owned on this server and a pawn. */
	APawn* OwnedPawnFor(const FString& Entity) const;
	/** Possess the entity's pawn if it is owned here and not possessed yet; restores PlayerState from the entity. */
	void Bind(APlayerController* PC, const FString& Entity);
	void RestoreState(APlayerController* PC, AActor* Actor, const FString& Entity);
	void SaveState(APlayerController* PC, AActor* Actor);

	UPROPERTY() TMap<TObjectPtr<AController>, FString> Entities;
	TMap<TWeakObjectPtr<AController>, FString> Saved;
	float Accum = 0.f;
};
