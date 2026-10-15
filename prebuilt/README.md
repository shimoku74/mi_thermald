# Android prebuilt

`mi_thermald` is the Android ARM64 release binary built from this repository with the Android NDK API 29 linker.

Build command:

```sh
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang \
  cargo build --release --locked --target aarch64-linux-android
install -m 0755 target/aarch64-linux-android/release/mi_thermald prebuilt/mi_thermald
```

SHA-256: `ef77bd5484bf9dc0a27033f88310cb77b4f036e903a43eecc116781f1e7f2bef`

Install as the device's thermal service executable with mode 0755 and the existing service label. Do not run two thermal daemons against the same cooling nodes.
