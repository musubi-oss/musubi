#pragma once

#include "Modules/ModuleManager.h"

class FMusubiMassModule : public IModuleInterface
{
public:
	virtual void StartupModule() override {}
	virtual void ShutdownModule() override {}
};
