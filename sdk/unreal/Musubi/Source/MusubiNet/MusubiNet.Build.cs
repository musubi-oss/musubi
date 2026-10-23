using UnrealBuildTool;

// Engine networking (UNetDriver/UNetConnection) carried through the Musubi node: a client's game
// connection goes to whichever simulator owns its entity, and follows it across handovers. Classic
// UNetDriver API only, so it builds on every tier; Iris/classic replication is the engine's choice.
public class MusubiNet : ModuleRules
{
	public MusubiNet(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Musubi", "MusubiServer" });
		PrivateDependencyModuleNames.AddRange(new string[] { "NetCore", "Sockets", "PacketHandler", "Json" });
	}
}
