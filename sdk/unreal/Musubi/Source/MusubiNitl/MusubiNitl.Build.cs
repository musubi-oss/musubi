using UnrealBuildTool;

// Node-in-the-loop harness (phase 9f): a pawn that walks across the cell border and a subsystem that drives a
// headless server/client from the command line so `tools/nitl.sh` can prove rehome against a live node. Ships
// in the plugin because it needs no project code; inert unless `-MusubiNitl=` is given.
public class MusubiNitl : ModuleRules
{
	public MusubiNitl(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Musubi", "MusubiServer", "MusubiNet" });
		PrivateDependencyModuleNames.AddRange(new string[] { "NetCore", "Json" });
	}
}
