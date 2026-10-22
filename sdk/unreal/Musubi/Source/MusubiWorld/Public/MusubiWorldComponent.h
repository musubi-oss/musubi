// The seamless-world client: drives `world_subscribe` from the owner's position, applies `world_delta`
// into an entity registry, and turns entities into pooled actors or ISM instances with interpolation
// that follows the server's time scale. Put one on the local pawn (or the camera owner).
#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "MusubiWire.generated.h"
#include "MusubiWorldTypes.h"
#include "MusubiEntityMapping.h"
#include "MusubiWorldComponent.generated.h"

class FMusubiSocket;
class UMusubiSubsystem;
class UMusubiEntityComponent;
class UInstancedStaticMeshComponent;

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnEntitySpawned, const FString&, EntityId, AActor*, Actor);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnEntityDespawned, const FString&, EntityId);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnTimeScaleChanged, float, TimeScale);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnWorldStats, const FMusubiWorldStats&, Stats);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiEntityRecordEvent, const FMusubiEntityRecord&, bool /*bSpawn (false = re-sync)*/);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiEntityGoneEvent, const FMusubiUuid&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiWorldDeltaEvent, const FMusubiServerMessageWorldDelta&);

UCLASS(ClassGroup = (Musubi), meta = (BlueprintSpawnableComponent))
class MUSUBIWORLD_API UMusubiWorldComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UMusubiWorldComponent();

	// ---- Settings ----
	/** World id/name on the node. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World") FString WorldName;
	/** Subscribe as soon as the socket is open. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World") bool bAutoSubscribe = true;
	/** Interest radius in cm; the effective radius is max(ViewDistance, 2 × CellSize), shrunk by the bandwidth guard. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "100")) float ViewDistance = 15000.f;
	/** Cell size of the world (cm), from the world's config on the node. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "0")) float CellSize = 5000.f;
	/** Smallest radius the bandwidth guard may shrink to. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "100")) float MinRadius = 3000.f;
	/** Re-send the subscription after moving this far (0 = radius × 0.25). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "0")) float ResubscribeDistance = 0.f;
	/** …or at least this often, so the node keeps the view fresh. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "0.5")) float ResubscribeIntervalSec = 5.f;
	/** Per-tick byte budget hint (0 = the world's default). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "0")) int32 ClientBudgetBytes = 0;
	/** Most deltas per second wanted (0 = min(target frame rate, 30)). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World", meta = (ClampMin = "0", ClampMax = "120")) int32 MaxHz = 0;
	/** Component-equality filter: every delivered entity must have these components with these JSON values. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World") TMap<FString, FString> Filter;
	/** Ask for position-only updates as unreliable sequenced frames (only useful on a datagram transport). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World") bool bUnreliableUpdates = false;
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World") TObjectPtr<UMusubiEntityMapping> Mapping;
	/** Render this many ticks behind the newest sample (2 @ 30 Hz = 66 ms). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation", meta = (ClampMin = "0", ClampMax = "10")) float InterpolationDelayTicks = 2.f;
	/** Extrapolate at most this far when samples run out. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation", meta = (ClampMin = "0", ClampMax = "5")) float MaxExtrapolateTicks = 1.f;
	/** Tick rate assumed until measured from the deltas. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation", meta = (ClampMin = "1", ClampMax = "240")) float NominalTickHz = 30.f;
	/** Component carrying the entity's height (JSON number, cm); missing → DefaultZ. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation") FString HeightComponent = TEXT("z");
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation") float DefaultZ = 0.f;
	/** Component carrying the yaw in degrees (JSON number); missing → face the movement direction. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Interpolation") FString YawComponent = TEXT("yaw");
	/** Despawned actors are kept hidden per class for reuse. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Sinks", meta = (ClampMin = "0")) int32 PoolSize = 64;
	/** Updates for an id never spawned wait this many ticks for the spawn before being dropped. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Advanced", meta = (ClampMin = "0")) int32 UnknownUpdateGraceTicks = 2;
	/** After a reconnect, entities the new keyframe did not mention are despawned after this many seconds. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Advanced", meta = (ClampMin = "0.1")) float ReconnectOrphanGraceSec = 1.5f;
	/** Bandwidth guard: shrink the radius ×0.8 while late/lost frames exceed this per second for 3 s; grow ×1.1/s when clean. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi|World|Advanced", meta = (ClampMin = "0")) float GuardLateFramesPerSec = 5.f;

	// ---- Control ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void Subscribe();
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void Unsubscribe();
	UFUNCTION(BlueprintPure, Category = "Musubi|World") bool IsSubscribed() const { return bSubscribed; }
	/** Container whose contents to see while closed (own inventory, the chest being browsed). */
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void OpenContainer(const FString& EntityId);
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void CloseContainer();
	/** The entity that is this client's own avatar; its actor is marked local and `SendInput` targets it. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void SetLocalEntity(const FString& EntityId);
	UFUNCTION(BlueprintPure, Category = "Musubi|World") FString GetLocalEntity() const { return LocalEntity.IsSet() ? LocalEntity->ToString() : FString(); }
	/** `world_input` for the local entity (fire-and-forget; the replication layer routes it to the owner). */
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") bool SendInputJson(const FString& InputJson);
	bool SendInput(const FMusubiJson& Input);
	/** Override the camera/interest position (default: owner actor location). */
	UFUNCTION(BlueprintCallable, Category = "Musubi|World") void SetInterestOverride(FVector WorldPos, bool bEnable);

	// ---- Query ----
	UFUNCTION(BlueprintPure, Category = "Musubi|World") FMusubiWorldStats GetStats() const { return Stats; }
	UFUNCTION(BlueprintPure, Category = "Musubi|World") int32 EntityCount() const { return Registry.Num(); }
	UFUNCTION(BlueprintPure, Category = "Musubi|World") AActor* FindEntityActor(const FString& EntityId) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|World") bool GetEntityPosition(const FString& EntityId, FVector& OutPos) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|World") TArray<FString> EntityIds() const;
	/** Entities within `Radius` of `Center`, nearest first. */
	UFUNCTION(BlueprintPure, Category = "Musubi|World") TArray<FString> EntitiesNear(FVector Center, float Radius) const;
	const FMusubiEntityRecord* FindRecord(const FMusubiUuid& Id) const { return Registry.Find(Id); }
	const TMap<FMusubiUuid, FMusubiEntityRecord>& Entities() const { return Registry; }
	/** Playback clock in authoritative ticks (fractional), i.e. what is being rendered right now. */
	double RenderTick() const { return PlaybackTick; }

	// ---- Events ----
	UPROPERTY(BlueprintAssignable, Category = "Musubi|World") FMusubiOnEntitySpawned OnEntitySpawned;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|World") FMusubiOnEntityDespawned OnEntityDespawned;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|World") FMusubiOnTimeScaleChanged OnTimeScaleChanged;
	/** Once per second. */
	UPROPERTY(BlueprintAssignable, Category = "Musubi|World") FMusubiOnWorldStats OnStats;
	FMusubiEntityRecordEvent OnRecordSpawned;
	FMusubiEntityGoneEvent OnRecordDespawned;
	/** Every applied delta, after the registry changed. */
	FMusubiWorldDeltaEvent OnDeltaApplied;

	// UActorComponent
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

private:
	// socket
	TWeakPtr<FMusubiSocket> Bound;
	FDelegateHandle DeltaHandle, ResubHandle, ClosedHandle;
	UMusubiSubsystem* Musubi() const;
	TSharedPtr<FMusubiSocket> Socket();
	void Bind();
	void Unbind();
	void HandleDelta(const FMusubiServerMessageWorldDelta& D);
	void HandleReconnect();
	void HandleClosed();

	// subscription driver
	bool bSubscribed = false, bWantSubscribed = false;
	FVector LastSentPos = FVector::ZeroVector;
	double LastSentAt = -1e9;
	float EffectiveRadius = 0.f;
	TOptional<FString> Inside;
	TOptional<FMusubiUuid> LocalEntity;
	bool bInterestOverride = false;
	FVector InterestOverride = FVector::ZeroVector;
	FVector InterestPos() const;
	float BaseRadius() const;
	void SendSubscribe(bool bForce);

	// registry
	TMap<FMusubiUuid, FMusubiEntityRecord> Registry;
	struct FPendingUpdate { FMusubiWorldEntity Entity; uint64 Tick = 0; uint64 ExpiresTick = 0; };
	TArray<FPendingUpdate> PendingUnknown;
	uint64 LastReliableTick = 0, LastSequencedTick = 0, LastTick = 0;
	bool bAwaitingKeyframe = false;
	double KeyframeDeadline = 0;
	void ApplySpawn(const FMusubiWorldEntity& E, uint64 Tick);
	bool ApplyUpdate(const FMusubiWorldEntity& E, uint64 Tick, bool bSequenced);
	void ApplyDespawn(const FMusubiUuid& Id);
	void ApplyComponents(FMusubiEntityRecord& R, const FMusubiWorldEntity& E, UMusubiEntityComponent* Comp);
	FVector WirePos(const FMusubiEntityRecord& R, const FMusubiWorldEntity& E) const;
	void ResetRegistry();

	// playback clock / interpolation
	double PlaybackTick = 0;
	double TickHz = 30.0;
	double LastDeltaWallTime = -1;
	uint64 LastDeltaTick = 0;
	float TimeScale = 1.f;
	void NoteTickTiming(uint64 Tick);
	void Interpolate(float DeltaTime);

	// sinks
	TMap<TObjectPtr<UClass>, TArray<TWeakObjectPtr<AActor>>> Pool;
	struct FInstancedSink { TWeakObjectPtr<UInstancedStaticMeshComponent> Ism; TArray<int32> FreeSlots; TArray<FMusubiUuid> SlotOwner; float Scale = 1.f; };
	TMap<TObjectPtr<UStaticMesh>, FInstancedSink> Instanced;
	TArray<FTransform> IsmScratch;
	void Materialize(FMusubiEntityRecord& R, const FMusubiWorldEntity& E);
	void Dematerialize(FMusubiEntityRecord& R);
	AActor* AcquireActor(UClass* Class, const FVector& At);
	void ReleaseActor(AActor* A);
	UMusubiEntityComponent* EntityComponentOf(AActor* A, bool bCreate);
	FInstancedSink& SinkFor(UStaticMesh* Mesh, float Scale);
	void FlushInstancedTransforms();

	// stats / guard
	FMusubiWorldStats Stats;
	int32 DeltasThisSec = 0, SequencedThisSec = 0, LateThisSec = 0;
	double StatsWindowStart = 0;
	float GuardBadSeconds = 0.f;
	void RollStats(double Now);
	void BandwidthGuard(float DeltaTime);
};
