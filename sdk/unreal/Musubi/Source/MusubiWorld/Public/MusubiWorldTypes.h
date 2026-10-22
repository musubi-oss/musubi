// Client-side view of one Musubi world: what the registry keeps per entity and the counters a HUD wants.
#pragma once

#include "CoreMinimal.h"
#include "MusubiWire.generated.h"
#include "MusubiWorldTypes.generated.h"

class AActor;
class UStaticMesh;

UENUM(BlueprintType)
enum class EMusubiEntitySink : uint8
{
	/** Spawn an actor (pooled) per entity. */
	Actor,
	/** One instance in a UInstancedStaticMeshComponent per mesh; for thousands of static-ish props. */
	Instanced,
	/** Keep the record (components, position) but render nothing. */
	None,
	/** Hand the entity to the Mass sink (`MusubiMass` module): a Mass entity with a transform fragment, no actor. */
	Mass,
};

/** One component of an entity as the client knows it. */
struct MUSUBIWORLD_API FMusubiEntityComponentState
{
	FMusubiJson Value;
	uint64 Version = 0;
};

/** A position sample; `Tick` is the authoritative tick it was produced at. */
struct MUSUBIWORLD_API FMusubiPosSample
{
	uint64 Tick = 0;
	FVector Pos = FVector::ZeroVector;
};

struct MUSUBIWORLD_API FMusubiEntityRecord
{
	FMusubiUuid Id;
	EMusubiEntitySink Sink = EMusubiEntitySink::None;
	TWeakObjectPtr<AActor> Actor;
	/** Instanced sink: mesh the instance lives in and its slot. */
	TWeakObjectPtr<UStaticMesh> Mesh;
	int32 InstanceSlot = INDEX_NONE;
	TMap<FString, FMusubiEntityComponentState> Components;
	/** Last 4 position samples, oldest first. */
	TArray<FMusubiPosSample> Ring;
	uint64 LastTick = 0;
	/** Set on reconnect; cleared by the first frame that mentions the entity again. */
	bool bStale = false;
	/** Where it is drawn this frame (interpolated), for sinks that need it after the fact. */
	FVector RenderPos = FVector::ZeroVector;

	const FMusubiEntityComponentState* FindComponent(const FString& Name) const { return Components.Find(Name); }
	void PushPos(uint64 Tick, const FVector& P);
	/** Position at fractional tick `T`: interpolates inside the ring, extrapolates at most `MaxExtrapolateTicks` past the newest sample. */
	FVector PosAt(double T, double MaxExtrapolateTicks) const;
};

USTRUCT(BlueprintType)
struct MUSUBIWORLD_API FMusubiWorldStats
{
	GENERATED_BODY()

	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int32 Entities = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int32 Actors = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int32 Instances = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int32 PooledActors = 0;
	/** Reliable deltas per second (last second). */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float DeltasPerSec = 0;
	/** Sequenced (unreliable) frames per second. */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float SequencedPerSec = 0;
	/** Sequenced frames that arrived older than what was already applied (lost or reordered). */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float LateFramesPerSec = 0;
	/** Measured authoritative tick rate from delta timestamps. */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float TickHz = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float TimeScale = 1;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") float Radius = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int64 LastTick = 0;
	/** Updates for ids the client never saw spawned that were dropped after the grace window. */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int64 DroppedUnknownUpdates = 0;
	/** Component updates ignored because a newer version was already applied (handover re-sends). */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") int64 StaleComponentUpdates = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi|World") bool bSubscribed = false;
};
