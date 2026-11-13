//! Windows builds: puts the application icon (packaging/windows/fastcnsl.ico)
//! into fastcnsl.exe, so Explorer, the taskbar, shortcuts and Settings → Apps
//! show it. Other targets: nothing to do.
//!
//! The resource is compiled with the MinGW `windres` of the target:
//! `x86_64-w64-mingw32-windres` or `aarch64-w64-mingw32-windres` (llvm-mingw),
//! looked up on PATH unless `WINDRES_<target>` names it, e.g.
//! `WINDRES_aarch64_pc_windows_gnullvm=/opt/llvm-mingw/bin/aarch64-w64-mingw32-windres`.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const RC: &str = "packaging/windows/fastcnsl.rc";
const ICO: &str = "packaging/windows/fastcnsl.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={RC}");
    println!("cargo:rerun-if-changed={ICO}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        println!("cargo:warning=no application icon: only MinGW targets (windows-gnu, windows-gnullvm) embed it");
        return;
    }

    let target = env::var("TARGET").expect("TARGET is set by cargo");
    let var = format!("WINDRES_{}", target.replace('-', "_"));
    println!("cargo:rerun-if-env-changed={var}");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH is set by cargo");
    let windres = env::var(&var).unwrap_or_else(|_| format!("{arch}-w64-mingw32-windres"));

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("fastcnsl-res.o");
    let status = Command::new(&windres)
        .args(["-I", "packaging/windows", RC, "-O", "coff", "-o"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("cannot run {windres} (set {var} to the MinGW windres): {e}"));
    if !status.success() {
        panic!("{windres} failed on {RC}: {status}");
    }
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
