using UnrealBuildTool;

public class MusubiWorld : ModuleRules
{
    public MusubiWorld(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "Musubi" });
    }
}
