using UnrealBuildTool;

public class MusubiServer : ModuleRules
{
    public MusubiServer(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "Musubi" });
        PrivateDependencyModuleNames.AddRange(new string[] { "HTTP", "WebSockets", "PhysicsCore" });
    }
}
