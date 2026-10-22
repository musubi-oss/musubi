using UnrealBuildTool;

public class Musubi : ModuleRules
{
    public Musubi(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "JsonUtilities" });
        PrivateDependencyModuleNames.AddRange(new string[] { "HTTP", "WebSockets", "DeveloperSettings" });
    }
}
