using UnrealBuildTool;

// Chaos Vehicles across a handover: the wheeled vehicle snapshot (gear, RPM, per-wheel suspension/rotation/steer/spin)
// rides in musubi.phys.ext. Drop this module from the .uplugin (and the ChaosVehiclesPlugin dependency) if a project
// has no vehicles.
public class MusubiVehicles : ModuleRules
{
    public MusubiVehicles(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "MusubiServer" });
        PrivateDependencyModuleNames.AddRange(new string[] { "ChaosVehicles", "ChaosVehiclesCore", "PhysicsCore" });
    }
}
