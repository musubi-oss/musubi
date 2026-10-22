#include "MusubiQuicModule.h"
#include "MusubiQuicTransport.h"
#include "MusubiTransport.h"
#if MUSUBI_WITH_QUIC
#include "musubi_quic.h"
#endif

DEFINE_LOG_CATEGORY(LogMusubiQuic);

static bool GMusubiQuicRegistered = false;

bool FMusubiQuicModule::IsAvailable()
{
	return GMusubiQuicRegistered;
}

void FMusubiQuicModule::StartupModule()
{
#if MUSUBI_WITH_QUIC
	const uint32 Abi = mq_abi_version();
	if (Abi != MQ_ABI_VERSION)
	{
		UE_LOG(LogMusubiQuic, Error, TEXT("musubi-quic-ffi ABI %u, plugin expects %u: QUIC transport disabled"), Abi, MQ_ABI_VERSION);
		return;
	}
	FMusubiTransportRegistry::Register(EMusubiTransport::Quic, []() -> TUniquePtr<IMusubiTransport> { return MakeUnique<FMusubiQuicTransport>(); });
	GMusubiQuicRegistered = true;
	UE_LOG(LogMusubiQuic, Log, TEXT("QUIC transport registered (musubi-quic-ffi abi %u)"), Abi);
#else
	UE_LOG(LogMusubiQuic, Log, TEXT("no musubi-quic-ffi library for this platform: WebSocket only"));
#endif
}

void FMusubiQuicModule::ShutdownModule()
{
	if (GMusubiQuicRegistered)
	{
		FMusubiTransportRegistry::Unregister(EMusubiTransport::Quic);
		GMusubiQuicRegistered = false;
	}
}

IMPLEMENT_MODULE(FMusubiQuicModule, MusubiQuic)
