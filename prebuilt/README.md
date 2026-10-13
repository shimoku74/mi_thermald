# Android prebuilt

`mi_thermald` is the Android ARM64 release binary built from this repository with the Android NDK API 30 linker.

Build command:

```sh
cargo build --release --target aarch64-linux-android --offline
install -m 0755 target/aarch64-linux-android/release/mi_thermald prebuilt/mi_thermald
```

SHA-256: `c77adc32f273266f68e49ef8bed2fdd7f461ddb174e9d9c858e51c695400bf7b`

Install as the device's thermal service executable with mode 0755 and the existing service label. Do not run two thermal daemons against the same cooling nodes.
