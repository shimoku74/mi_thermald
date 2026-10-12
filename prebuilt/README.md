# Android prebuilt

`mi_thermald` is the Android ARM64 release binary built from this repository with the Android NDK API 30 linker.

Build command:

```sh
cargo build --release --target aarch64-linux-android --offline
install -m 0755 target/aarch64-linux-android/release/mi_thermald prebuilt/mi_thermald
```

SHA-256: `c4b993de83c953aa7397d84b3343a4443fa633a1470f6480f5534027684fd5fb`

Install as the device's thermal service executable with mode 0755 and the existing service label. Do not run two thermal daemons against the same cooling nodes.
