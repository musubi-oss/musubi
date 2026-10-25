using UnrealBuildTool;

// Node-in-the-loop harness (phase 9f): a pawn that walks across the cell border and a subsystem that drives a
// headless server/client from the command line so `tools/nitl.sh` can prove rehome against a live node. Ships
// in the plugin because it needs no project code; inert unless `-MusubiNitl=` is given.
public class MusubiNitl : ModuleRules
{
	public MusubiNitl(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "PhysicsCore", "Musubi", "MusubiServer", "MusubiNet" });
		PrivateDependencyModuleNames.AddRange(new string[] { "NetCore", "Json" });
		// The inventory FastArray needs the UHT-generated Iris binding (UE_NET_HAS_IRIS_FASTARRAY_BINDING) or the descriptor
		// builder drops it ("FFastArray property not registered"); same tier gate as MusubiIris.
		if (Target.Version.MajorVersion == 5 && Target.Version.MinorVersion >= 5) SetupIrisSupport(Target);
	}
}
