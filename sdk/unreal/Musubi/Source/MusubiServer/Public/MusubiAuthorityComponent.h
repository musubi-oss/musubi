// On every actor the simulator module manages: the Musubi entity behind it, whether this process
// owns it (authoritative, physics on) or sees it as a ghost of a neighbouring cell (kinematic,
// never written), its components as JSON, and the project hooks for inputs, interactions and
// handover consent.
#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "MusubiSimWire.generated.h"
#include "MusubiAuthorityComponent.generated.h"

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnAuthorityComponentChanged, const FString&, Name, const FString&, ValueJson);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnAuthorityChanged, bool, bOwned);
/** Native: a component value arrived from the node (after it is stored). */
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiOnComponentApplied, const FString& /*Name*/, const FMusubiJson& /*Value*/);

UCLASS(ClassGroup = (Musubi), meta = (BlueprintSpawnableComponent))
class MUSUBISERVER_API UMusubiAuthorityComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UMusubiAuthorityComponent() { SetIsReplicatedByDefault(true); }
	/** The entity id as replicated to engine clients (MusubiNet): lets a client recognise the same entity again after a rehome. */
	UPROPERTY(ReplicatedUsing = OnRep_NetEntityId, Transient, BlueprintReadOnly, Category = "Musubi|Authority") FString NetEntityId;
	UFUNCTION() void OnRep_NetEntityId();
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	/** Replicated to engine clients: true on the actor of the owning server, false on a relayed ghost of another server's entity. */
	UPROPERTY(Replicated, Transient, BlueprintReadOnly, Category = "Musubi|Authority") bool bNetOwned = false;
	/** Client side: an actor replicated by a server identified itself with its entity id. */
	DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiOnClientEntityIdentified, AActor*, const FString&);
	static FMusubiOnClientEntityIdentified OnClientEntityIdentified;
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") FString GetEntityId() const { return EntityId.ToString(); }
	const FMusubiUuid& GetEntityUuid() const { return EntityId; }
	/** True while this process is the authority (owned entity in a leased cell). */
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") bool IsOwned() const { return bOwned; }
	/** True for an entity of a neighbouring cell mirrored here for collisions; read-only. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") bool IsGhost() const { return !bOwned; }
	/** True between HandoverStarted and HandoverCompleted: frozen, do not write. */
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") bool IsInHandover() const { return bInHandover; }
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") FString GetCell() const { return FString::Printf(TEXT("%d,%d"), Cell.x, Cell.y); }
	const FMusubiCellId& GetCellId() const { return Cell; }

	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") bool HasComponent(const FString& Name) const { return Components.Contains(Name); }
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") FString GetComponentJson(const FString& Name) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") double GetComponentNumber(const FString& Name, double Default = 0.0) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") FString GetComponentString(const FString& Name, const FString& Default = "") const;
	const FMusubiJson* FindComponent(const FString& Name) const;
	UFUNCTION(BlueprintPure, Category = "Musubi|Authority") TArray<FString> ComponentNames() const;

	/** Owned only: set a component; it goes to the node with the next `apply`. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void SetComponentJson(const FString& Name, const FString& ValueJson);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void SetComponentNumber(const FString& Name, double Value);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void SetComponentString(const FString& Name, const FString& Value);
	void SetComponent(const FString& Name, const FMusubiJson& Value);
	/** Owned only: ask the node to despawn this entity. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void RequestDespawn();

	/** Replicated subobjects (`AddReplicatedSubObject`) the engine keeps in a private list: register them here too so their
	 *  replicated state rides in the Musubi handover beside the actor and its components. The object must have a stable
	 *  name under this actor (`NewObject<T>(Actor, TEXT("Loadout"))`) and the receiving server must register the same
	 *  name — the owner's state is applied to the receiver's instance by name, like a replicated component's. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void RegisterMusubiSubObject(UObject* SubObject);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void UnregisterMusubiSubObject(UObject* SubObject);
	const TArray<TWeakObjectPtr<UObject>>& MusubiSubObjects() const { return SubObjects; }
	UObject* FindMusubiSubObject(const FString& Name) const;
	/** Owned only: report a contact/interaction with another entity (usually a ghost); the node routes it to the owner. */
	UFUNCTION(BlueprintCallable, Category = "Musubi|Authority") void Interact(const FString& Kind, const FString& PayloadJson, const FString& TargetEntityId = "");

	// ---- project hooks (override in C++ or Blueprint) ----
	/** A player input routed to this entity (also buffered ones replayed after a handover). */
	UFUNCTION(BlueprintNativeEvent, Category = "Musubi|Authority") void ApplyInput(const FString& InputJson);
	virtual void ApplyInput_Implementation(const FString& InputJson) {}
	/** A cross-cell interaction targeting this entity (`kind` + payload from the other simulator). */
	UFUNCTION(BlueprintNativeEvent, Category = "Musubi|Authority") void OnInteraction(const FString& Kind, const FString& PayloadJson, const FString& FromEntityId);
	virtual void OnInteraction_Implementation(const FString& Kind, const FString& PayloadJson, const FString& FromEntityId) {}
	/** Return false to refuse a handover (e.g. mid-interaction); the node rolls it back after its timeout. */
	UFUNCTION(BlueprintNativeEvent, Category = "Musubi|Authority") bool CanRelease();
	virtual bool CanRelease_Implementation() { return true; }

	UPROPERTY(BlueprintAssignable, Category = "Musubi|Authority") FMusubiOnAuthorityComponentChanged OnComponentChanged;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Authority") FMusubiOnAuthorityChanged OnAuthorityChanged;
	FMusubiOnComponentApplied OnComponentApplied;

	/** Component names the module writes itself on handover (velocity restore); projects should not use them. */
	static const TCHAR* VelocityComponent() { return TEXT("musubi.vel"); }
	static const TCHAR* AngularVelocityComponent() { return TEXT("musubi.ang"); }
	/** Full rigid-body state for the handover: {q:[x,y,z,w], v, w, sleep, sim, broken:[constraint names]}. */
	static const TCHAR* PhysicsComponent() { return TEXT("musubi.phys"); }
	/** Rotation the owner last captured into `musubi.phys` (ghosts apply it). */
	bool GetPhysicsRotation(FQuat& Out) const;

	// ---- driven by UMusubiSimulatorSubsystem ----
	void Bind(class UMusubiSimulatorSubsystem* Owner, const FMusubiUuid& Id, const FMusubiCellId& InCell, bool bIsOwned);
	void SetCell(const FMusubiCellId& InCell) { Cell = InCell; }
	void SetOwned(bool bIsOwned);
	void SetInHandover(bool b) { bInHandover = b; }
	/** From the node: only versions newer than what we have are applied; returns true when something changed. */
	bool ApplyFromNode(const FMusubiWorldEntity& E);
	/** Owned: components changed locally since the last `apply`, as one JSON object (null when none). */
	TSharedPtr<FJsonObject> TakeDirty();
	bool HasDirty() const { return Dirty.Num() > 0; }
	/** Snapshot the physics state into components so the receiver resumes exactly (handover). */
	void CapturePhysicsIntoComponents();
	/** Whether this body simulates physics by nature (a prop), as opposed to a kinematic one moved by a movement component
	 *  (a Character capsule): remembered the first time we touch it, so the authority code never switches simulation on for
	 *  a body that never had it — a simulating capsule falls through the world and CharacterMovement stops moving it. */
	bool BodySimulates();
	bool bBodySimulates = true;
	bool bBodyKnown = false;
	/** Restore the physics state from components after arrival. */
	void RestorePhysicsFromComponents();
	void SetPhysicsEnabled(bool bEnable);
	FVector LastSentPos = FVector::ZeroVector;
	uint64 LastTick = 0;

private:
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Sim;
	FMusubiUuid EntityId;
	FMusubiCellId Cell;
	bool bOwned = false;
	bool bInHandover = false;
	struct FState { FMusubiJson Value; uint64 Version = 0; };
	TMap<FString, FState> Components;
	TSet<FString> Dirty;
	TArray<TWeakObjectPtr<UObject>> SubObjects;
	class UPrimitiveComponent* Body() const;
};
