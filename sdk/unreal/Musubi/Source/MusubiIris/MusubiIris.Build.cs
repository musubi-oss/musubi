using UnrealBuildTool;

// Tier B only (5.5+): Iris bridge. On older engines the module compiles to an empty stub.
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
		}
		else
		{
			PublicDefinitions.Add("MUSUBI_WITH_IRIS=0");
		}
	}
}
