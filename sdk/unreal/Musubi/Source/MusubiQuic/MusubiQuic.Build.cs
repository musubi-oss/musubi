using System.IO;
using UnrealBuildTool;

// QUIC transport (RFC 9000) for FMusubiSocket: links the Rust `musubi-quic-ffi` static library
// (quinn + rustls, the same stack as the node) and registers EMusubiTransport::Quic. When no
// prebuilt library exists for the target platform the module compiles to a stub that registers nothing.
public class MusubiQuic : ModuleRules
{
    public MusubiQuic(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "Musubi", "Json" });

        string ThirdParty = Path.Combine(ModuleDirectory, "..", "ThirdParty", "MusubiQuicFfi");
        PublicIncludePaths.Add(Path.Combine(ThirdParty, "include"));

        string Lib = null;
        if (Target.Platform == UnrealTargetPlatform.Win64)
            Lib = Path.Combine(ThirdParty, "lib", "Win64", "musubi_quic_ffi.lib");
        else if (Target.Platform == UnrealTargetPlatform.Linux || Target.Platform == UnrealTargetPlatform.LinuxArm64)
            Lib = Path.Combine(ThirdParty, "lib", Target.Platform == UnrealTargetPlatform.Linux ? "Linux" : "LinuxArm64", "libmusubi_quic_ffi.a");
        else if (Target.Platform == UnrealTargetPlatform.Mac)
            Lib = Path.Combine(ThirdParty, "lib", "Mac", "libmusubi_quic_ffi.a");

        if (Lib != null && File.Exists(Lib))
        {
            PublicAdditionalLibraries.Add(Lib);
            if (Target.Platform == UnrealTargetPlatform.Win64)
            {
                // Rust std + ring/quinn (mio) system imports.
                PublicSystemLibraries.AddRange(new string[] { "ws2_32.lib", "userenv.lib", "bcrypt.lib", "ntdll.lib", "advapi32.lib", "ole32.lib", "oleaut32.lib", "iphlpapi.lib", "runtimeobject.lib", "synchronization.lib" });
            }
            else if (Target.Platform == UnrealTargetPlatform.Linux || Target.Platform == UnrealTargetPlatform.LinuxArm64)
            {
                PublicSystemLibraries.AddRange(new string[] { "pthread", "dl", "m" });
            }
            PublicDefinitions.Add("MUSUBI_WITH_QUIC=1");
        }
        else
        {
            PublicDefinitions.Add("MUSUBI_WITH_QUIC=0");
        }
    }
}
