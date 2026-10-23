#pragma once

#include "CoreMinimal.h"
#include "Engine/NetDriver.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "MusubiMeshNetDriver.generated.h"

/**
 * Phase 9d: RPCs that the engine cannot deliver locally — `Server`/`NetMulticast` calls on ghosts, `Client` calls
 * for a player whose controller lives on another server — go through the node to the owning server.
 *
 * A second, connection-less net driver (`MusubiMesh`): `AActor::CallRemoteFunction` offers every RPC to every
 * active driver whose `ShouldReplicateFunction` says yes, so this one picks the calls on actors that carry a
 * `UMusubiAuthorityComponent` and are ghosts here (or are multicasts) and sends them as
 * `interact{kind:"rpc", payload:{f: function path, c: class path, b: base64 params, bits}}` — the node routes
 * an interaction to the entity's current owner, in order per entity. The owner's `UMusubiMeshRpcSubsystem`
 * decodes the parameters and calls `ProcessEvent`. Parameters use the classic property net serialization
 * without a package map, so object references are not carried (null on arrival). Multicasts reach the owner,
 * whose own engine replication relays them to its clients; other servers' ghosts do not receive them (v1).
 */
UCLASS(transient, config = Engine)
class MUSUBINET_API UMusubiMeshNetDriver : public UNetDriver
{
	GENERATED_BODY()
public:
	UMusubiMeshNetDriver(const FObjectInitializer& OI);
	static const FName DriverName;      // "MusubiMesh"
	static const FName DefinitionName;  // "MusubiMeshNetDriver" (NetDriverDefinitions)

	virtual bool IsAvailable() const override { return true; }
	virtual bool InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error) override;
	virtual bool InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error) override;
	virtual void TickDispatch(float DeltaTime) override {}
	virtual void LowLevelSend(TSharedPtr<const FInternetAddr> Address, void* Data, int32 CountBits, FOutPacketTraits& Traits) override {}
	virtual FString LowLevelGetNetworkNumber() override { return TEXT("musubi-mesh"); }
	virtual bool IsNetResourceValid() override { return true; }
	virtual class ISocketSubsystem* GetSocketSubsystem() override { return nullptr; }
	virtual bool ShouldReplicateActor(AActor* Actor) const override { return false; }
	virtual bool ShouldReplicateFunction(AActor* Actor, UFunction* Function) const override;
	virtual void ProcessRemoteFunction(AActor* Actor, UFunction* Function, void* Parms, FOutParmRec* OutParms, FFrame* Stack, UObject* SubObject = nullptr) override;

	/** Classic property net serialization of the function's parameters (no package map: no object references). */
	static bool EncodeParams(UFunction* Function, void* Parms, TArray<uint8>& OutBytes, int64& OutBits);
	static bool DecodeAndCall(UObject* Target, UFunction* Function, const TArray<uint8>& Bytes, int64 Bits);

	int32 Sent = 0, Dropped = 0;
};

/** Dedicated servers: creates the `MusubiMesh` driver while the simulator runs and executes RPCs the node delivers. */
UCLASS()
class MUSUBINET_API UMusubiMeshRpcSubsystem : public UGameInstanceSubsystem
{
	GENERATED_BODY()
public:
	virtual bool ShouldCreateSubsystem(UObject* Outer) const override;
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;
	virtual void Deinitialize() override;
	int32 Executed = 0, Rejected = 0;

private:
	void OnStarted();
	void OnStopped();
	void OnRpc(AActor* Actor, const struct FMusubiJson& Payload);
	TWeakObjectPtr<class UMusubiSimulatorSubsystem> Sim;
	FDelegateHandle StartedHandle, StoppedHandle, RpcHandle, WorldHandle;
};
