#include "MusubiModule.h"
#include "WebSocketsModule.h"

DEFINE_LOG_CATEGORY(LogMusubi);

void FMusubiModule::StartupModule()
{
    FModuleManager::Get().LoadModuleChecked("WebSockets");
}

void FMusubiModule::ShutdownModule() {}

IMPLEMENT_MODULE(FMusubiModule, Musubi)
