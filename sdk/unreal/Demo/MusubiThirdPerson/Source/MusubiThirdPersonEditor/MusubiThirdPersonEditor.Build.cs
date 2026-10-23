using UnrealBuildTool;

// Editor-only: the commandlet that authors the demo's Blueprint logic headlessly (no interactive editor on a build box).
public class MusubiThirdPersonEditor : ModuleRules
{
	public MusubiThirdPersonEditor(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine" });
		PrivateDependencyModuleNames.AddRange(new string[] { "UnrealEd", "BlueprintGraph", "KismetCompiler", "Kismet", "MusubiThirdPerson", "MusubiServer" });
	}
}
