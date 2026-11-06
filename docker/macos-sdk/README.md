# macOS SDK for cross-compiling

`make macos-arm64` needs Apple's macOS SDK. Apple's license doesn't allow
redistributing it, so it isn't in this repository and must be supplied by
whoever builds the macOS package, from a Mac with Xcode or the Command Line
Tools installed:

```sh
git clone https://github.com/tpoechtrager/osxcross
cd osxcross
./tools/gen_sdk_package.sh        # writes MacOSX<version>.sdk.tar.xz
```

Copy the resulting `MacOSX<version>.sdk.tar.xz` into this folder. It is
git-ignored. SDK 13 or newer is recommended.

Alternatively, build natively on a Mac: `cargo build --release` produces
`target/release/ragamuffin`, and `scripts/package/macos-arm64.sh` bundles it
into `Ragamuffin.app` (it needs `rcodesign` or Apple's `codesign` on PATH).
