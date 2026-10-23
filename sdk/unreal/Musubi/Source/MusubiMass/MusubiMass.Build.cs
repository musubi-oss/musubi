using UnrealBuildTool;

// Mass sink for the world view: entities routed to EMusubiEntitySink::Mass become Mass entities
// (FTransformFragment + FMusubiEntityFragment) instead of actors. MassEntity is an engine module from
// 5.5 and the plugin of the same name before that; MassCommon (FTransformFragment) is in MassGameplay.
public class MusubiMass : ModuleRules
{
    public MusubiMass(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Musubi", "MusubiWorld", "MassEntity", "MassCommon" });
        PrivateDependencyModuleNames.AddRange(new string[] { "Json" });
        // FStructTracker / FStructTypeBitSet (used inline by MassEntity headers) live in the StructUtils
        // plugin up to 5.4 (seen as link errors on 5.3.2); from 5.5 they are in CoreUObject.
        if (Target.Version.MajorVersion == 5 && Target.Version.MinorVersion < 5)
        {
            PrivateDependencyModuleNames.Add("StructUtils");
        }
    }
}
