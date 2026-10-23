using UnrealBuildTool;
using System.Collections.Generic;

public class MusubiThirdPersonTarget : TargetRules
{
	public MusubiThirdPersonTarget(TargetInfo Target) : base(Target)
	{
		Type = TargetType.Game;
		DefaultBuildSettings = BuildSettingsVersion.Latest;
		IncludeOrderVersion = EngineIncludeOrderVersion.Latest;
		ExtraModuleNames.Add("MusubiThirdPerson");
	}
}
