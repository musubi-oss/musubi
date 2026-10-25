#include "MusubiServerModule.h"
#include "MusubiSimulatorSettings.h"
#include "HAL/IConsoleManager.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"
#include "Modules/ModuleManager.h"

DEFINE_LOG_CATEGORY(LogMusubiServer);

void FMusubiServerModule::StartupModule()
{
	// UWorldPartition::IsServerStreamingEnabled resolves wp.Runtime.EnableServerStreaming once, at world init, so the plugin
	// has to set it before the map loads; a value from an ini or the command line has a higher priority and stays.
	const UMusubiSimulatorSettings* S = GetDefault<UMusubiSimulatorSettings>();
	const bool bServer = IsRunningDedicatedServer() || FParse::Param(FCommandLine::Get(), TEXT("server"));
	if (!S || !S->bWorldPartitionStreaming || !bServer) return;
	for (const TCHAR* Name : { TEXT("wp.Runtime.EnableServerStreaming"), TEXT("wp.Runtime.EnableServerStreamingOut") })
	{
		if (IConsoleVariable* V = IConsoleManager::Get().FindConsoleVariable(Name)) V->Set(1, ECVF_SetByProjectSetting);
	}
	UE_LOG(LogMusubiServer, Log, TEXT("world partition server streaming on (leased cells + margin are this server's streaming source)"));
}

IMPLEMENT_MODULE(FMusubiServerModule, MusubiServer)
