using UnrealBuildTool;
using System.Collections.Generic;

public class MusubiThirdPersonServerTarget : TargetRules
{
	public MusubiThirdPersonServerTarget(TargetInfo Target) : base(Target)
	{
		Type = TargetType.Server;
		DefaultBuildSettings = BuildSettingsVersion.Latest;
		IncludeOrderVersion = EngineIncludeOrderVersion.Latest;
		ExtraModuleNames.Add("MusubiThirdPerson");
	}
}
