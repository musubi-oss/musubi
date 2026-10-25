// Headless Unreal as a Musubi simulator (docs/gdk-unreal.md §6): one process, one world, a set of
// leased cells. Frames from the node become owned actors (authoritative, physics on) and ghosts
// (neighbouring cells, kinematic); every tick the changed owned actors go back as `apply` and the
// tick is closed with `tick_done`, which also renews the lease. Events drive handover, inputs,
// interactions and time dilation.
#pragma once

#include "CoreMinimal.h"
#include "MusubiCellLevelStreamer.h"
#include "Containers/Ticker.h"
#include "Engine/EngineBaseTypes.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "MusubiSimWire.generated.h"
#include "MusubiSimStream.h"
#include "WorldPartition/WorldPartitionStreamingSource.h"
#include "MusubiSimulatorSubsystem.generated.h"

class UMusubiAuthorityComponent;
class UMusubiSimulatorSettings;
class ULevelStreamingDynamic;

inline uint32 GetTypeHash(const FMusubiCellId& C) { return HashCombine(HashCombine(::GetTypeHash(C.x), ::GetTypeHash(C.y)), HashCombine(::GetTypeHash(C.z), ::GetTypeHash((int32)C.level))); }
inline bool operator==(const FMusubiCellId& A, const FMusubiCellId& B) { return A.x == B.x && A.y == B.y && A.z == B.z && A.level == B.level; }

USTRUCT(BlueprintType)
struct MUSUBISERVER_API FMusubiSimulatorStats
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") bool bConnected = false;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int32 Cells = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int32 Owned = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int32 Ghosts = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 Tick = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") float FramesPerSec = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") float TicksPerSec = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") float AppliesPerSec = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") float SlowestTimeScale = 1;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 HandoversOut = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 HandoversIn = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 Queued = 0;
};

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnCellChanged, const FString&, Cell, bool, bAssigned);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnSimEntity, const FString&, EntityId, AActor*, Actor);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnHandover, const FString&, EntityId, const FString&, Phase, const FString&, Detail);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnCellTimeScale, const FString&, Cell, float, Scale);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnQueueEvent, const FString&, Cell, const FString&, Ticket, const FString&, Detail);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FMusubiOnTxEvent, const FString&, Tx, const FString&, EntityId, const FString&, Outcome);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnSimulatorStats, const FMusubiSimulatorStats&, Stats);
/** Project hook: choose/spawn the actor for an entity (return null to use the settings' class). */
DECLARE_DELEGATE_RetVal_ThreeParams(AActor*, FMusubiSpawnEntityActor, const FMusubiWorldEntity&, bool /*bOwned*/, const FTransform&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiSimEventDelegate, const FMusubiSimEvent&);

UCLASS()
class MUSUBISERVER_API UMusubiSimulatorSubsystem : public UGameInstanceSubsystem, public IWorldPartitionStreamingSourceProvider
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;
	virtual void Deinitialize() override;

	// ---- control ----
	/** Connect to the node and start ticking at `Hz` (settings from UMusubiSimulatorSettings unless overridden before). */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Simulator") void Start();
	UFUNCTION(BlueprintCallable, Category = "Musubi|Simulator") void Stop();
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") bool IsRunning() const { return bRunning; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") int32 GetHz() const { return Hz; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") FString GetSimulatorId() const { return SimId.ToString(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") FMusubiSimulatorStats GetStats() const { return Stats; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") TArray<FString> LeasedCells() const;
	/** The world's grid (from the node at start): base cell side in cm, 0 until known. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") float WorldCellSize() const { return (float)GridCellSize; }
	/** World-space bounds of a cell ("x,y" or "x,y,z@level"); invalid until the grid is known. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") FBox CellBounds(const FString& Cell) const;
	// IWorldPartitionStreamingSourceProvider: one source, a sphere per leased cell (UMusubiSimulatorSettings::bWorldPartitionStreaming)
	virtual bool GetStreamingSource(FWorldPartitionStreamingSource& OutSource) const override;
	virtual const UObject* GetStreamingSourceOwner() const override { return this; }
	/** Override settings before Start (tests, command line). */
	void Configure(const FString& ApiBase, const FString& ServerKey, const FString& World, int32 Capacity, int32 Hz);

	// ---- commands ----
	/** Ask the node for more cells (it may refuse or redirect; the result arrives as CellAssigned events / the reply). */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Simulator") void LeaseCells(const TArray<FString>& Cells);
	/** Spawn an entity we will own; the actor appears when the node confirms (OnEntitySpawned). */
	/** Spawn an entity owned by this server's cell. `Owner` (account id, optional) makes it that player's avatar: only their sessions may relay/input on it, and `world_join` finds it. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Simulator") void SpawnEntity(double X, double Y, const FString& ComponentsJson, bool bPersistent = false, bool bGlobal = false, const FString& Owner = TEXT(""));
	void Spawn(double X, double Y, const FMusubiJson& Components, bool bPersistent, bool bGlobal, bool bQueue, TOptional<uint64> DecayTicks, FMusubiSimReply OnReply = nullptr, TOptional<FMusubiUuid> Owner = TOptional<FMusubiUuid>());
	void Despawn(const FMusubiUuid& Entity, FMusubiSimReply OnReply = nullptr);
	void Attach(const FMusubiUuid& Entity, const FMusubiUuid& Root, bool bContained, FMusubiSimReply OnReply = nullptr);
	void Detach(const FMusubiUuid& Entity, FMusubiSimReply OnReply = nullptr);
	void Interact(const FMusubiUuid& Entity, const FString& Kind, const FMusubiJson& Payload, const FMusubiUuid& From, FMusubiSimReply OnReply = nullptr);
	void Colocate(const FMusubiUuid& Entity, const FMusubiUuid& With, uint64 TtlTicks = 300, FMusubiSimReply OnReply = nullptr);
	void Rewind(const FMusubiUuid& Entity, uint64 Tick, TOptional<FVector2D> At, TOptional<double> Tolerance, FMusubiSimReply OnReply = nullptr);
	/** Typed rewind: node answer (pos at tick, hit) plus the local ring's view of the same tick for owned entities (nullopt when not ours / out of history). */
	void RewindChecked(const FMusubiUuid& Entity, uint64 Tick, TOptional<FVector2D> At, TOptional<double> Tolerance, TFunction<void(bool bOk, const FMusubiRewindResult& Node, TOptional<FVector> Local)> OnReply);
	/** Where an owned entity was at `Tick` per the local history ring (lerp between the two nearest ticks). No round trip. */
	bool LocalRewind(const FMusubiUuid& Entity, uint64 Tick, FVector& OutPos, FVector& OutVel) const;
	/** Oldest tick the ring still holds for the entity (0 = none). */
	uint64 HistoryFrom(const FMusubiUuid& Entity) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") ULevelStreamingDynamic* CellLevel(const FString& Cell) const;
	/** Set a world-global component value on an entity we own and flush it now (otherwise `apply` goes with the tick). */
	void FlushNow();

	// ---- query ----
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") AActor* FindEntityActor(const FString& EntityId) const;
	UMusubiAuthorityComponent* FindAuthority(const FMusubiUuid& Id) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") TArray<AActor*> OwnedActors() const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Simulator") float CellTimeScale(const FString& Cell) const;
	TSharedPtr<FMusubiSimStream> Stream() const { return Link; }

	// ---- events ----
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnCellChanged OnCellChanged;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnSimEntity OnEntitySpawned;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnSimEntity OnEntityDespawned;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnSimEntity OnGhostSpawned;
	/** Phase: request | started | arrived | completed | cancelled. */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnHandover OnHandover;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnCellTimeScale OnCellTimeScale;
	/** Detail: queued:<pos>[/eta] | update:<pos> | admitted | expired:<reason> | full:<cap> */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnQueueEvent OnQueueEvent;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnTxEvent OnTxEvent;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Simulator") FMusubiOnSimulatorStats OnStats;
	FMusubiSpawnEntityActor SpawnEntityActor;
	/** Every event, after the module handled it. */
	FMusubiSimEventDelegate OnSimEvent;
	/** Start of every simulator tick (before owned actors are collected into `apply`): exporters hook here. */
	FSimpleMulticastDelegate OnSimTicked;
	/** The simulator started / stopped (C++ listeners such as the Iris exporter). */
	FSimpleMulticastDelegate OnStarted, OnStopped;
	/** An engine RPC routed by the node (`interact` kind `rpc`) for one of our actors: payload = {f, c, b, bits, from}. MusubiNet executes it. */
	DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiOnSimRpc, AActor*, const FMusubiJson&);
	FMusubiOnSimRpc OnRpc;

	/** Engine packet relay (`MusubiNet`): answer a `net_packet` event on the same conn/session. */
	void NetSendPacket(uint64 Conn, const FMusubiJson& Session, TArray<uint8>&& Data);
	/** Control-lane bytes for the client behind `Conn` (`net_meta`): reliable, ordered before our later packets. */
	void NetSendMeta(uint64 Conn, const FMusubiJson& Session, TArray<uint8>&& Data);
	/** Close engine connection `Conn` on `Entity`; the client gets `net_close`. */
	void NetClose(uint64 Conn, const FMusubiJson& Session, const FMusubiUuid& Entity, const FString& Reason);

private:
	struct FCell
	{
		FMusubiCellId Id;
		uint64 Tick = 0;
		float TimeScale = 1.f;
		bool bHibernated = false;
		TSet<FMusubiUuid> Owned, Ghosts;
	};
	struct FSnapshot
	{
		uint64 Tick = 0;
		FVector Pos = FVector::ZeroVector;
		FVector Vel = FVector::ZeroVector;
	};
	struct FEntity
	{
		TWeakObjectPtr<AActor> Actor;
		TWeakObjectPtr<UMusubiAuthorityComponent> Auth;
		FMusubiCellId Cell;
		bool bOwned = false;
		/** Ring of the last HistoryTicks owned states, oldest first once full (Head = next write slot). */
		TArray<FSnapshot> History;
		int32 Head = 0;
		bool bFull = false;
		void Record(uint64 Tick, const FVector& P, const FVector& V, int32 Cap);
		const FSnapshot* Newest() const;
	};
	int32 HistoryTicks = 64;
	/** Level instances of the leased cells + the ring around them (node `levels`, else `CellLevels`). */
	FMusubiCellLevelStreamer CellLevels;
	void RefreshCellLevels(const TCHAR* Why);

	TSharedPtr<FMusubiSimStream> Link;
	FTSTicker::FDelegateHandle TickerHandle;
	FDelegateHandle PostActorTickHandle;
	void PreReplicateGhosts(UWorld* W, ELevelTick TickType, float DeltaSeconds);
	FMusubiUuid SimId;
	bool bRunning = false;
	FString ApiBase, ServerKey, WorldName;
	int32 Capacity = 4, Hz = 30;
	bool bDelta = true;
	bool bCbor = true;
	float PositionEpsilon = 0.5f;
	FString HeightComponent;
	TArray<FString> InitialCells;
	TMap<FMusubiCellId, FCell> Cells;
	TSet<FMusubiCellId> Leased;
	double GridCellSize = 0, GridGhostMargin = 0;
	bool bGridThreeD = false;
	double NextGridFetch = 0;
	TWeakObjectPtr<class UWorldPartitionSubsystem> WpSubsystem;
	bool bWpFilterBound = false;
	void FetchWorldGrid();
	void UpdateWorldPartitionSource();
	void UnregisterWorldPartitionSource();
	FBox BoundsOf(const FMusubiCellId& C) const;
	TMap<FMusubiUuid, FEntity> Entities;
	TArray<FMusubiSimFrameTick> Inbox;
	uint64 LastTick = 0;
	double Accum = 0;
	FMusubiSimulatorStats Stats;
	int32 FramesThisSec = 0, TicksThisSec = 0, AppliesThisSec = 0;
	double StatsWindowStart = 0;

	void LoadSettings();
	bool EngineTick(float DeltaSeconds);
	void SimTick();
	void ApplyFrame(const FMusubiSimFrameTick& F);
	void ApplyCell(const FMusubiCellFrame& C, const TSet<FMusubiUuid>& InFrame, const TSet<FMusubiUuid>& OwnedInFrame);
	void HandleEvent(const FMusubiSimEvent& E);
	void UpsertEntity(const FMusubiWorldEntity& E, const FMusubiCellId& Cell, bool bOwned, uint64 Tick);
	void RemoveEntity(const FMusubiUuid& Id, const TCHAR* Why);
	void CollectAndApply();
	AActor* SpawnActorFor(const FMusubiWorldEntity& E, bool bOwned, const FVector& At);
	FVector WirePos(const FMusubiWorldEntity& E, const FVector& Fallback) const;
	void SendCommand(FMusubiSimCommand&& Cmd, FMusubiSimReply OnReply = nullptr);
	void RollStats(double Now);
	static FString CellKey(const FMusubiCellId& C) { return FString::Printf(TEXT("%d,%d"), C.x, C.y); }
	static bool ParseCell(const FString& S, FMusubiCellId& Out);
};
