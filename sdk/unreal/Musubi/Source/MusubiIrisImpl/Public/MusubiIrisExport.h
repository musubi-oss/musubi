#pragma once

#include "CoreMinimal.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "Iris/ReplicationSystem/NetRefHandle.h"
#include "Iris/ReplicationSystem/NetObjectFactoryRegistry.h"
#include "Runtime/Launch/Resources/Version.h"
#if ENGINE_MINOR_VERSION >= 7
#include "Iris/ReplicationSystem/NetTokenStore.h"
#endif
#include "MusubiIrisExport.generated.h"

class UReplicationSystem;
class UMusubiReplicationBridge;
class UMusubiSimulatorSubsystem;
class AActor;

/**
 * Gives the Musubi Iris bridge a replication system of its own (docs/gdk-unreal.md §9.3). The engine's
 * NetDriver bridge is `UEngineReplicationBridge`, which is `final` and instantiated by the driver, so
 * authority → node export cannot ride on it: this subsystem creates a second `UReplicationSystem` whose
 * bridge is `UMusubiReplicationBridge`, registers every owned entity actor as a root object when the
 * simulator spawns it, and ticks the system once per simulator tick (NetUpdate → TickPostReceive →
 * SendUpdate → PostSendUpdate, the sequence UNetDriver uses) so the bridge's PreSendUpdate sees polled,
 * quantized state and writes the `iris` component. Dedicated servers only.
 */
UCLASS()
class UMusubiIrisExportSubsystem : public UGameInstanceSubsystem
{
	GENERATED_BODY()
public:
	virtual bool ShouldCreateSubsystem(UObject* Outer) const override;
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;
	virtual void Deinitialize() override;

	UReplicationSystem* GetReplicationSystem() const { return System; }
	int32 NumRootObjects() const { return Handles.Num(); }

private:
	void OnStarted();
	void OnStopped();
	void OnTick();
	UFUNCTION() void OnEntitySpawned(const FString& EntityId, AActor* Actor);
	UFUNCTION() void OnEntityDespawned(const FString& EntityId, AActor* Actor);
	UFUNCTION() void OnGhostSpawned(const FString& EntityId, AActor* Actor);

	UPROPERTY() TObjectPtr<UReplicationSystem> System;
	UPROPERTY() TObjectPtr<UMusubiReplicationBridge> Bridge;
#if ENGINE_MINOR_VERSION >= 7
	/** 5.7+: UReplicationSystem::Init refuses to start without a NetTokenStore carrying FString/FName token stores (UNetDriver builds the same one). */
	TUniquePtr<UE::Net::FNetTokenStore> NetTokenStore;
#endif
	TWeakObjectPtr<UMusubiSimulatorSubsystem> Sim;
	TMap<TWeakObjectPtr<AActor>, UE::Net::FNetRefHandle> Handles;
	UE::Net::FNetObjectFactoryId FactoryId = UE::Net::InvalidNetObjectFactoryId;
	FDelegateHandle StartedHandle, StoppedHandle, TickHandle;
};
