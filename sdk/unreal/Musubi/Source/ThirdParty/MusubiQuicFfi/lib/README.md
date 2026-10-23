Prebuilt `musubi-quic-ffi` static libraries, one directory per UBT platform:

```
Win64/musubi_quic_ffi.lib
Linux/libmusubi_quic_ffi.a
Mac/libmusubi_quic_ffi.a
```

Build with `sdk/unreal/tools/build-quic-ffi.sh [--target <triple>]` (runs `cargo build -p musubi-quic-ffi --release`
and copies the artifact here, built without LTO/embedded bitcode and with debug info stripped: UE's Linux link step runs
Breakpad `dump_syms` on every .so and the 5.3 binary aborts on the DWARF 5 that Rust std carries). Without a library for the target platform `MusubiQuic.Build.cs` skips the module's
native half and the plugin stays WebSocket-only.
