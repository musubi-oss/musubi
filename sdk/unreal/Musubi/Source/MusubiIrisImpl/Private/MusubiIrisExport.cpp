#include "MusubiIrisExport.h"
#include "Runtime/Launch/Resources/Version.h"

#include "Engine/GameInstance.h"
#include "GameFramework/Actor.h"
#include "Iris/ReplicationSystem/ObjectReplicationBridge.h"
#include "Iris/ReplicationSystem/ReplicationBridgeTypes.h"
#include "Iris/ReplicationSystem/ReplicationSystem.h"
#include "Iris/ReplicationSystem/ReplicationView.h"
#if ENGINE_MINOR_VERSION >= 7
#include "Iris/ReplicationSystem/NameTokenStore.h"
#include "Iris/ReplicationSystem/StringTokenStore.h"
#endif
#include "Misc/EngineVersionComparison.h"
#include "Modules/ModuleManager.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiIrisModule.h"
#include "MusubiNetObjectFactory.h"
#include "MusubiReplicationBridge.h"
#include "MusubiSimulatorSubsystem.h"

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 8
using FMusubiRootParams = UE::Net::FRootObjectReplicationParams;
#else
using FMusubiRootParams = UObjectReplicationBridge::FRootObjectReplicationParams;
#endif

bool UMusubiIrisExportSubsystem::ShouldCreateSubsystem(UObject* Outer) const
{
	return IsRunningDedicatedServer() && Super::ShouldCreateSubsystem(Outer);
}

void UMusubiIrisExportSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);
	UMusubiSimulatorSubsystem* S = Collection.InitializeDependency<UMusubiSimulatorSubsystem>();
	if (!S) return;
	Sim = S;
	StartedHandle = S->OnStarted.AddUObject(this, &UMusubiIrisExportSubsystem::OnStarted);
	StoppedHandle = S->OnStopped.AddUObject(this, &UMusubiIrisExportSubsystem::OnStopped);
	S->OnEntitySpawned.AddDynamic(this, &UMusubiIrisExportSubsystem::OnEntitySpawned);
	S->OnEntityDespawned.AddDynamic(this, &UMusubiIrisExportSubsystem::OnEntityDespawned);
	S->OnGhostSpawned.AddDynamic(this, &UMusubiIrisExportSubsystem::OnGhostSpawned);
	if (S->IsRunning()) OnStarted();
}

void UMusubiIrisExportSubsystem::Deinitialize()
{
	OnStopped();
	if (UMusubiSimulatorSubsystem* S = Sim.Get())
	{
		S->OnStarted.Remove(StartedHandle);
		S->OnStopped.Remove(StoppedHandle);
		S->OnEntitySpawned.RemoveDynamic(this, &UMusubiIrisExportSubsystem::OnEntitySpawned);
		S->OnEntityDespawned.RemoveDynamic(this, &UMusubiIrisExportSubsystem::OnEntityDespawned);
	}
	Super::Deinitialize();
}

void UMusubiIrisExportSubsystem::OnStarted()
{
	if (System) return;
	UMusubiSimulatorSubsystem* S = Sim.Get();
	if (!S) return;
	FactoryId = UE::Net::FNetObjectFactoryRegistry::GetFactoryIdFromName(UMusubiNetObjectFactory::FactoryName);
	Bridge = NewObject<UMusubiReplicationBridge>(this);
	// With a classic GameNetDriver the engine never starts the IrisCore *module*: its .so is only pulled in as a
	// library dependency, so FIrisCoreModule::StartupModule (which binds NetCore's Iris push-model delegates) never
	// runs. Our root objects carry Iris push ids, and the classic driver's MARK_PROPERTY_DIRTY on them would then
	// Execute an unbound delegate (SIGSEGV in UEPushModelPrivate::MarkPropertyDirty on 5.8).
	FModuleManager::LoadModuleChecked<IModuleInterface>(TEXT("IrisCore"));
	UReplicationSystem::FReplicationSystemParams Params;
	Params.ReplicationBridge = Bridge;
	Params.bIsServer = true;
	Params.bAllowObjectReplication = true;
#if ENGINE_MINOR_VERSION >= 7
	NetTokenStore = MakeUnique<UE::Net::FNetTokenStore>();
	UE::Net::FNetTokenStore::FInitParams TokenParams;
	TokenParams.Authority = UE::Net::FNetToken::ENetTokenAuthority::Authority;
	TokenParams.MaxConnections = 2;
	NetTokenStore->Init(TokenParams);
	NetTokenStore->CreateAndRegisterDataStore<UE::Net::FStringTokenStore>();
	NetTokenStore->CreateAndRegisterDataStore<UE::Net::FNameTokenStore>();
	Params.NetTokenStore = NetTokenStore.Get();
#endif
	System = UE::Net::FReplicationSystemFactory::CreateReplicationSystem(Params);
	if (!System)
	{
		UE_LOG(LogMusubiIris, Error, TEXT("could not create the export ReplicationSystem"));
		Bridge = nullptr;
		return;
	}
	// one pseudo connection that sees everything: nothing is written to a wire, the bridge does the
	// work in PreSendUpdate, but Iris only polls/quantizes state that some connection could receive
	System->AddConnection(1);
	UE::Net::FReplicationView View;
	View.Views.AddDefaulted(); // position/direction only; no filters are configured on this system, so one view sees all
	System->SetReplicationView(1, View);
	TickHandle = S->OnSimTicked.AddUObject(this, &UMusubiIrisExportSubsystem::OnTick);
	UE_LOG(LogMusubiIris, Log, TEXT("export ReplicationSystem %u up (bridge %s)"), System->GetId(), *Bridge->GetName());
}

void UMusubiIrisExportSubsystem::OnStopped()
{
	if (UMusubiSimulatorSubsystem* S = Sim.Get()) S->OnSimTicked.Remove(TickHandle);
	Handles.Reset();
	if (System)
	{
		UE::Net::FReplicationSystemFactory::DestroyReplicationSystem(System);
		System = nullptr;
	}
	Bridge = nullptr;
#if ENGINE_MINOR_VERSION >= 7
	NetTokenStore.Reset();
#endif
}

void UMusubiIrisExportSubsystem::OnTick()
{
	if (!System) return;
	UMusubiSimulatorSubsystem* S = Sim.Get();
	const float Dt = S && S->GetHz() > 0 ? 1.0f / (float)S->GetHz() : 1.0f / 30.0f;
	// UNetDriver's order (5.7/5.8): PostDispatchSendUpdate = TickPostReceive → SendUpdate → PostSendUpdate (the
	// PostTickDispatch pass), then InternalIrisUpdate = NetUpdate (opens the TickFlush pass) and PostTickFlush =
	// PostSendUpdate (closes it). TickPostReceive ensures no pass is open, so NetUpdate must not come first.
	System->TickPostReceive();
	System->SendUpdate([](TArrayView<uint32>) {});
	System->PostSendUpdate();
	System->NetUpdate(Dt);
	System->PostSendUpdate();
}

void UMusubiIrisExportSubsystem::OnEntitySpawned(const FString& EntityId, AActor* Actor)
{
	if (!System || !Bridge || !Actor) return;
	UMusubiAuthorityComponent* Auth = Actor->FindComponentByClass<UMusubiAuthorityComponent>();
	if (!Auth || !Auth->IsOwned() || Handles.Contains(Actor)) return;
	Bridge->DisablePushDirtiness(Actor->GetClass());
#if ENGINE_MAJOR_VERSION > 5 || ENGINE_MINOR_VERSION >= 8
	// 5.8: the root params moved to UNetObjectFactory; the overload taking them is deprecated
	const UE::Net::FNetRefHandle H = Bridge->StartReplicatingRootObject(Actor, FactoryId);
#else
	FMusubiRootParams P;
	P.bNeedsWorldLocationUpdate = true;
	const UE::Net::FNetRefHandle H = Bridge->StartReplicatingRootObject(Actor, P, FactoryId);
#endif
	if (!H.IsValid())
	{
		UE_LOG(LogMusubiIris, Warning, TEXT("entity %s: Iris refused %s as a root object"), *EntityId, *Actor->GetName());
		return;
	}
	Handles.Add(Actor, H);
	Bridge->Track(H, Actor);
}

void UMusubiIrisExportSubsystem::OnEntityDespawned(const FString& EntityId, AActor* Actor)
{
	if (!Bridge || !Actor) return;
	UE::Net::FNetRefHandle H;
	if (Handles.RemoveAndCopyValue(Actor, H) && H.IsValid()) Bridge->StopReplicatingNetRefHandle(H, EEndReplicationFlags::Destroy);
}

// Ghosts: every `iris` value the node delivers for this actor goes through the bridge into its UPROPERTYs.
void UMusubiIrisExportSubsystem::OnGhostSpawned(const FString&, AActor* Actor)
{
	if (!Actor) return;
	UMusubiAuthorityComponent* Auth = Actor->FindComponentByClass<UMusubiAuthorityComponent>();
	if (!Auth) return;
	TWeakObjectPtr<AActor> WeakActor = Actor;
	TWeakObjectPtr<UMusubiIrisExportSubsystem> WeakSelf = this;
	Auth->OnComponentApplied.AddLambda([WeakActor, WeakSelf](const FString& Name, const FMusubiJson& Value)
	{
		if (Name != TEXT("iris") || !WeakActor.IsValid() || !WeakSelf.IsValid() || !WeakSelf->Bridge) return;
		UMusubiAuthorityComponent* A = WeakActor->FindComponentByClass<UMusubiAuthorityComponent>();
		if (!A || A->IsOwned()) return; // we are the authority now; the state is ours
		const TSharedPtr<FJsonObject>* Obj = nullptr;
		if (Value.Value.IsValid() && Value.Value->TryGetObject(Obj) && Obj) WeakSelf->Bridge->ApplyBinary(WeakActor.Get(), *Obj, WeakSelf->Sim.Get());
	});
	if (const FMusubiJson* Have = Auth->FindComponent(TEXT("iris")))
	{
		const TSharedPtr<FJsonObject>* Obj = nullptr;
		if (Bridge && Have->Value.IsValid() && Have->Value->TryGetObject(Obj) && Obj) Bridge->ApplyBinary(Actor, *Obj, Sim.Get());
	}
}
