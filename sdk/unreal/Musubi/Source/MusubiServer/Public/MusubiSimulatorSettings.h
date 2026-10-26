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
	/** Ticks of owned-entity history kept per entity (position + velocity) for LocalRewind and handover-cancel restore. 64 @ 30 Hz ≈ 2 s. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation", meta = (ClampMin = "2", ClampMax = "1024")) int32 HistoryTicks = 64;
	/** Stream a level instance per leased cell: key = cell id as "x,y", value = the cell's static level. Loaded on CellAssigned, unloaded on revoke/release. */
	UPROPERTY(Config, EditAnywhere, Category = "Levels") bool bStreamCellLevels = true;
	UPROPERTY(Config, EditAnywhere, Category = "Levels") TMap<FString, TSoftObjectPtr<UWorld>> CellLevels;
	/** World offset of a cell level = cell index × this (cm); 0 → every level loads at the origin (levels already placed in world space). */
	UPROPERTY(Config, EditAnywhere, Category = "Levels") float CellLevelStride = 0.f;
	/** Ask the node for per-cell deltas instead of full views every tick. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation") bool bDeltaFrames = true;
	/** Simulator stream as CBOR binary WebSocket messages (iris blobs as raw bytes) instead of JSON text; see docs §9.9. */
	UPROPERTY(Config, EditAnywhere, Category = "Simulation") bool bBinaryStream = true;
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
	/** Ghosts live as ROLE_SimulatedProxy on this server (HasAuthority() false, CharacterMovement in simulated mode, Server RPCs routed to the owner)
	 *  and still replicate to this server's clients as simulated proxies (role swap on replicate; 5.8+: EnableReplicationOfActorsWithAnyRole).
	 *  Off = ghosts keep ROLE_Authority like before (ask IsGhost()). Ignored on an Iris GameNetDriver before 5.8, which cannot replicate non-authority actors. */
	UPROPERTY(Config, EditAnywhere, Category = "Actors") bool bGhostsAsSimulatedProxy = true;
	/** World Partition on a dedicated server: this simulator is a streaming source covering its leased cells (+ margin), so the
	 *  engine loads/unloads the map's WP cells under split/merge/handover by itself (one map for the whole world, no travel).
	 *  Turns on wp.Runtime.EnableServerStreaming(+Out) at module start unless an ini/command line already set them. */
	UPROPERTY(Config, EditAnywhere, Category = "World Partition") bool bWorldPartitionStreaming = true;
	/** Uncooked servers (editor -server) have no WP cell packages and would close a client that streams a cell this server
	 *  has not: set net.SkipMissingLevelDisconnect so it is a warning instead. Cooked servers are unaffected. */
	UPROPERTY(Config, EditAnywhere, Category = "World Partition") bool bSkipMissingLevelDisconnectUncooked = true;
	/** Extra radius (cm) streamed around each leased cell; < 0 = the world's ghost_margin (where ghosts of neighbouring cells live). */
	UPROPERTY(Config, EditAnywhere, Category = "World Partition") float CellStreamingMargin = -1.f;
	/** Ignore player controllers as server streaming sources (clients stream by their own controller; the server streams by its cells). */
	UPROPERTY(Config, EditAnywhere, Category = "World Partition") bool bIgnorePlayerStreamingSources = true;
	/** Component carrying height (cm) for actors; empty = keep the actor's Z. */
	UPROPERTY(Config, EditAnywhere, Category = "Actors") FString HeightComponent = TEXT("z");
};
