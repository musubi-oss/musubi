Prebuilt `musubi-quic-ffi` static libraries, one directory per UBT platform:

```
Win64/musubi_quic_ffi.lib
Linux/libmusubi_quic_ffi.a
Mac/libmusubi_quic_ffi.a
```

Build with `sdk/unreal/tools/build-quic-ffi.sh [--target <triple>]` (runs `cargo build -p musubi-quic-ffi --release`
and copies the artifact here). Without a library for the target platform `MusubiQuic.Build.cs` skips the module's
native half and the plugin stays WebSocket-only.
