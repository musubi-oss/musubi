using UnrealBuildTool;
using System.Collections.Generic;

public class MusubiThirdPersonEditorTarget : TargetRules
{
	public MusubiThirdPersonEditorTarget(TargetInfo Target) : base(Target)
	{
		Type = TargetType.Editor;
		DefaultBuildSettings = BuildSettingsVersion.Latest;
		IncludeOrderVersion = EngineIncludeOrderVersion.Latest;
		ExtraModuleNames.AddRange(new string[] { "MusubiThirdPerson", "MusubiThirdPersonEditor" });
	}
}
