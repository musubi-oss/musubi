// `[/Script/MusubiServer.MusubiSimulatorSettings]` in DefaultGame.ini (or MusubiServer.ini on the
// box): how this dedicated-server build registers with the node as a simulator.
#pragma once

#include "CoreMinimal.h"
#include "Engine/DeveloperSettings.h"
#include "MusubiSimulatorSettings.generated.h"

UCLASS(Config = Game, DefaultConfig, meta = (DisplayName = "Musubi Simulator"))
class MUSUBISERVER_API UMusubiSimulatorSettings : public UDeveloperSettings
{
	GENERATED_BODY()
public:
	virtual FName GetCategoryName() const override { return TEXT("Plugins"); }

	/** Node base URL, e.g. https://musubi.example.com */
	UPROPERTY(Config, EditAnywhere, Category = "Node") FString ApiBase = TEXT("http://127.0.0.1:8080");
	/** Fleet/server key (`x-musubi-server-key`). Prefer the MUSUBI_SERVER_KEY environment variable on real boxes. */
	UPROPERTY(Config, EditAnywhere, Category = "Node") FString ServerKey;
	/** World this process simulates. */
	UPROPERTY(Config, EditAnywhere, Category = "Node") FString World;
	/** Cells this simulator can own at once (the node assigns up to this many). */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation", meta = (ClampMin = "1")) int32 Capacity = 4;
	/** Simulation rate; the node expects `tick_done` at this cadence and measures lag against it. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation", meta = (ClampMin = "1", ClampMax = "240")) int32 Hz = 30;
	/** Ask the node for per-cell deltas instead of full views every tick. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation") bool bDeltaFrames = true;
	/** Cells to lease at start as "x,y" (empty = wait for the node to assign). */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation") TArray<FString> InitialCells;
	/** Start the loop as soon as the game instance exists (dedicated server builds). */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation") bool bAutoStart = true;
	/** Minimum movement (cm) before an owned actor's position is sent in `apply`. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation", meta = (ClampMin = "0")) float PositionEpsilon = 0.5f;
	/** Actor spawned for owned entities without a project-provided class (needs a UMusubiAuthorityComponent; added if missing). */
	UPROPERTY(Config, EditAnywhere, Category = "Actors") TSoftClassPtr<AActor> OwnedActorClass;
	/** Actor spawned for ghosts (entities of neighbouring cells, kinematic). Empty = same as OwnedActorClass. */
	UPROPERTY(Config, EditAnywhere, Category = "Actors") TSoftClassPtr<AActor> GhostActorClass;
	/** Component carrying height (cm) for actors; empty = keep the actor's Z. */
	UPROPERTY(Config, EditAnywhere, Category = "Actors") FString HeightComponent = TEXT("z");
};
