#include "MusubiIrisModule.h"
#include "Misc/EngineVersionComparison.h"

#if MUSUBI_WITH_IRIS
#include "MusubiNetObjectFactory.h"
#include "MusubiActorFactory.h"
#include "MusubiSubObjectFactory.h"
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
#include "Net/Iris/ReplicationSystem/EngineReplicationBridge.h"
#include "Net/Iris/ReplicationSystem/NetActorFactory.h"
#include "Net/Iris/ReplicationSystem/NetSubObjectFactory.h"
#endif
#include "MusubiEntityRegistry.h"
#include "Iris/ReplicationSystem/NetObjectFactoryRegistry.h"
#endif

DEFINE_LOG_CATEGORY(LogMusubiIris);

void FMusubiIrisModule::StartupModule()
{
#if MUSUBI_WITH_IRIS
	// 5.7 NetObjectFactoryRegistry.h: static RegisterFactory(UClass*, FName) / UnregisterFactory(FName)
	UE::Net::FNetObjectFactoryRegistry::RegisterFactory(UMusubiNetObjectFactory::StaticClass(), UMusubiNetObjectFactory::FactoryName);
	UE_LOG(LogMusubiIris, Log, TEXT("Iris bridge enabled (factory %s)"), *UMusubiNetObjectFactory::FactoryName.ToString());
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
	// entity actors on the engine's bridge (server → client): AActor::FillReplicationParams picks this factory by name
	UE::Net::FNetObjectFactoryRegistry::RegisterFactory(UMusubiActorFactory::StaticClass(), UMusubiEntityRegistry::IrisNetFactoryName);
	UE::Net::FNetObjectFactoryRegistry::RegisterFactory(UMusubiSubObjectFactory::StaticClass(), UMusubiEntityRegistry::IrisSubObjectFactoryName);
	// Config/DefaultEngine.ini makes them the bridge's default factories ([/Script/Engine.EngineReplicationBridge]), but
	// engine-module CDOs read their config before plugin ini files are merged into the hierarchy: re-read it now so the
	// section applies (a project's own DefaultEngine.ini still wins, it is in the same hierarchy)
	if (UObject* BridgeCDO = UEngineReplicationBridge::StaticClass()->GetDefaultObject())
	{
		BridgeCDO->ReloadConfig();
		// still the engine's factory (the plugin ini is not in this project's hierarchy): set ours; a project that chose
		// another factory keeps it
		auto Default = [BridgeCDO](const TCHAR* Prop, FName EngineName, FName MusubiName)
		{
			FNameProperty* P = FindFProperty<FNameProperty>(BridgeCDO->GetClass(), Prop);
			FName* V = P ? P->ContainerPtrToValuePtr<FName>(BridgeCDO) : nullptr;
			if (V && *V == EngineName) *V = MusubiName;
			UE_LOG(LogMusubiIris, Log, TEXT("engine bridge %s=%s"), Prop, V ? *V->ToString() : TEXT("?"));
		};
		Default(TEXT("ActorFactoryName"), UNetActorFactory::GetFactoryName(), UMusubiEntityRegistry::IrisNetFactoryName);
		Default(TEXT("SubObjectFactoryName"), UNetSubObjectFactory::GetFactoryName(), UMusubiEntityRegistry::IrisSubObjectFactoryName);
	}
#endif
#else
	UE_LOG(LogMusubiIris, Log, TEXT("Iris bridge disabled on this engine version (tier A: classic replication)"));
#endif
}

void FMusubiIrisModule::ShutdownModule()
{
#if MUSUBI_WITH_IRIS
	UE::Net::FNetObjectFactoryRegistry::UnregisterFactory(UMusubiNetObjectFactory::FactoryName);
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
	UE::Net::FNetObjectFactoryRegistry::UnregisterFactory(UMusubiEntityRegistry::IrisNetFactoryName);
	UE::Net::FNetObjectFactoryRegistry::UnregisterFactory(UMusubiEntityRegistry::IrisSubObjectFactoryName);
#endif
#endif
}

IMPLEMENT_MODULE(FMusubiIrisModule, MusubiIris)
