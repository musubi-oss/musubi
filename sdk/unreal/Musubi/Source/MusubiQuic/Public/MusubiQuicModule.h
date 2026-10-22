#pragma once

#include "CoreMinimal.h"
#include "Modules/ModuleManager.h"

DECLARE_LOG_CATEGORY_EXTERN(LogMusubiQuic, Log, All);

class FMusubiQuicModule : public IModuleInterface
{
public:
	virtual void StartupModule() override;
	virtual void ShutdownModule() override;
	/** True when the native library is linked and the transport is registered. */
	static bool IsAvailable();
};
