#pragma once

#include "CoreMinimal.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "Containers/Ticker.h"
#include "Engine/EngineBaseTypes.h"
#include "MusubiEntityRegistry.h"
#include "MusubiTypes.h"
#include "MusubiNitlSubsystem.generated.h"

/**
 * `-MusubiNitl=server`: once the simulator runs, `-MusubiNitlSpawn=<x>,<y>` spawns one entity (logged as
 * `nitl: spawned <uuid>`); handover phases are logged. `-MusubiNitl=client`: device auth → socket →
 * `musubi://<-MusubiWorld>?entity=<-MusubiEntity>`; the actors the engine replicates are logged once a second
 * and every adoption (`OnClientEntityIdentified`). `tools/nitl.sh` greps these lines.
 */
UCLASS()
class MUSUBINITL_API UMusubiNitlSubsystem : public UGameInstanceSubsystem
{
	GENERATED_BODY()
public:
	virtual bool ShouldCreateSubsystem(UObject* Outer) const override;
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;
	virtual void Deinitialize() override;

private:
	void OnSimStarted();
	UFUNCTION() void OnCellChanged(const FString& Cell, bool bAssigned);
	UFUNCTION() void OnHandover(const FString& EntityId, const FString& Phase, const FString& Detail);
	UFUNCTION() void OnSession(bool bOk, const FMusubiSession& Session);
	UFUNCTION() void OnSocketConnected();
	UFUNCTION() void OnSocketClosed(int32 Code, const FString& Reason);
	bool Report(float Dt);

	FString Mode;
	bool bTravelled = false, bSpawned = false;
	TWeakObjectPtr<class UMusubiSimulatorSubsystem> Sim;
	TWeakObjectPtr<class UMusubiSubsystem> Client;
	FDelegateHandle IdentifiedHandle;
	FMusubiEntityHandle Handle; // client: taken from the first identified pawn, must keep resolving across the rehome
	FDelegateHandle TickHandle;
	void OnWorldTick(UWorld* World, ELevelTick TickType, float DeltaSeconds);
	FTSTicker::FDelegateHandle Ticker;
};
