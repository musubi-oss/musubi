#include "MusubiIrisModule.h"

#if MUSUBI_WITH_IRIS
#include "MusubiNetObjectFactory.h"
#include "Iris/ReplicationSystem/NetObjectFactoryRegistry.h"
#endif

DEFINE_LOG_CATEGORY(LogMusubiIris);

void FMusubiIrisModule::StartupModule()
{
#if MUSUBI_WITH_IRIS
	// 5.7 NetObjectFactoryRegistry.h: static RegisterFactory(UClass*, FName) / UnregisterFactory(FName)
	UE::Net::FNetObjectFactoryRegistry::RegisterFactory(UMusubiNetObjectFactory::StaticClass(), UMusubiNetObjectFactory::FactoryName);
	UE_LOG(LogMusubiIris, Log, TEXT("Iris bridge enabled (factory %s)"), *UMusubiNetObjectFactory::FactoryName.ToString());
#else
	UE_LOG(LogMusubiIris, Log, TEXT("Iris bridge disabled on this engine version (tier A: classic replication)"));
#endif
}

void FMusubiIrisModule::ShutdownModule()
{
#if MUSUBI_WITH_IRIS
	UE::Net::FNetObjectFactoryRegistry::UnregisterFactory(UMusubiNetObjectFactory::FactoryName);
#endif
}

IMPLEMENT_MODULE(FMusubiIrisModule, MusubiIris)
