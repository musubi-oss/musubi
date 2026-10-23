using System.IO;
using UnrealBuildTool;
using EpicGames.Core;

// Tier B only (5.5+): Iris bridge. On older engines the module compiles to an empty stub: the bridge/factory
// sources live in ../MusubiIrisImpl (not a module) and are only added to this module on 5.5+, so UHT on
// 5.3/5.4 never parses classes whose base (UObjectReplicationBridge) does not exist there.
// Verified against 5.7 UBT: SetupIrisSupport() unconditionally adds IrisCore + UE_WITH_IRIS=1
// (no per-target bUseIris switch exists), so the only gate is the engine version.
public class MusubiIris : ModuleRules
{
	public MusubiIris(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Json", "Musubi", "MusubiServer", "MusubiWorld" });
		bool bIris = Target.Version.MajorVersion == 5 && Target.Version.MinorVersion >= 5;
		if (bIris)
		{
			PrivateDependencyModuleNames.Add("NetCore");
			SetupIrisSupport(Target);
			PublicDefinitions.Add("MUSUBI_WITH_IRIS=1");
			ConditionalAddModuleDirectory(new DirectoryReference(Path.Combine(ModuleDirectory, "..", "MusubiIrisImpl")));
		}
		else
		{
			PublicDefinitions.Add("MUSUBI_WITH_IRIS=0");
		}
	}
}
