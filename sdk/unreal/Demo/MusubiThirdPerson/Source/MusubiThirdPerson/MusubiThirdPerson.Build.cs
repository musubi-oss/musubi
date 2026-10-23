using UnrealBuildTool;

public class MusubiThirdPerson : ModuleRules
{
	public MusubiThirdPerson(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicIncludePaths.Add(ModuleDirectory); // headers sit in the module root (template layout); the editor module includes them
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "InputCore", "EnhancedInput", "UMG", "Musubi", "MusubiServer", "MusubiNet" });
		PrivateDependencyModuleNames.AddRange(new string[] { "NetCore" });
		// FastArray in the inventory needs the UHT Iris binding on 5.5+ (see docs/gdk-unreal.md §9.8)
		if (Target.Version.MajorVersion == 5 && Target.Version.MinorVersion >= 5) SetupIrisSupport(Target);
	}
}
