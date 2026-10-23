using UnrealBuildTool;

public class Musubi : ModuleRules
{
    public Musubi(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        // HTTP is public: MusubiClient.h (public) includes Interfaces/IHttpRequest.h.
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "JsonUtilities", "HTTP" });
        PrivateDependencyModuleNames.AddRange(new string[] { "WebSockets", "DeveloperSettings" });
    }
}
