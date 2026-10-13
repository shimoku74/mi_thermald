# Android prebuilt

`mi_thermald` is the Android ARM64 release binary built from this repository with the Android NDK API 30 linker.

Build command:

```sh
cargo build --release --target aarch64-linux-android --offline
install -m 0755 target/aarch64-linux-android/release/mi_thermald prebuilt/mi_thermald
```

SHA-256: `9b4a8eb535f65a9b00c8fd7227ab970eecfb044e00a94d80bcf32d77709d7468`

Install as the device's thermal service executable with mode 0755 and the existing service label. Do not run two thermal daemons against the same cooling nodes.
