/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/main.slint").unwrap();

    // Linux: transcribe-cpp is a shared libtranscribe plus loadable ggml modules. `$ORIGIN` first so
    // `target/debug/iris` runs directly; the .deb puts the libs in `/usr/lib/iris`, the AppImage in `usr/lib`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/iris:$ORIGIN/../lib");
    }

    stage_transcribe_runtime_libs();
    // After the transcribe staging, which recreates transcribe-libs/.
    stage_vc_runtime_dlls();
}

/// From Handy's build.rs (MIT): with `IRIS_VC_REDIST_DIRS` set (packaging), copies the VC++ runtime DLLs the native
/// code imports into `transcribe-libs/`, so the installer ships them next to the exe.
fn stage_vc_runtime_dlls() {
    println!("cargo:rerun-if-env-changed=IRIS_VC_REDIST_DIRS");
    let Some(redist_dirs) = std::env::var_os("IRIS_VC_REDIST_DIRS") else {
        return;
    };
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("transcribe-libs");
    std::fs::create_dir_all(&dest).expect("create transcribe-libs staging dir");

    let mut copied: Vec<String> = Vec::new();
    for dir in std::env::split_paths(&redist_dirs) {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("IRIS_VC_REDIST_DIRS: read {}: {e}", dir.display()))
            .flatten()
        {
            let src = entry.path();
            let lower = src.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            // transcribe-cpp 0.4.2 imports no vcomp140 (S3).
            let wanted =
                lower.ends_with(".dll") && (lower.starts_with("msvcp140") || lower.starts_with("vcruntime140"));
            if wanted {
                std::fs::copy(&src, dest.join(&lower)).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
                copied.push(lower);
            }
        }
    }
    for required in ["msvcp140.dll", "vcruntime140.dll"] {
        if !copied.iter().any(|n| n == required) {
            panic!("IRIS_VC_REDIST_DIRS is set but {required} was not found in it");
        }
    }
    println!("cargo:warning=Staged {} VC++ runtime DLL(s) for app-local deployment", copied.len());
}

/// From Handy's build.rs (MIT): copies transcribe-cpp's shared runtime libraries and dlopen'd ggml backend modules
/// (`DEP_TRANSCRIBE_CPP_{RUNTIME,MODULE}_DIR`) into `transcribe-libs/` for packaging. On Linux also next to the binary
/// (`target/<profile>/`, found through the `$ORIGIN` rpath), so `target/debug/iris` runs without `cargo run`'s
/// `LD_LIBRARY_PATH`; on Windows the sys crate already copies its DLLs there.
fn stage_transcribe_runtime_libs() {
    println!("cargo:rerun-if-env-changed=DEP_TRANSCRIBE_CPP_RUNTIME_DIR");
    println!("cargo:rerun-if-env-changed=DEP_TRANSCRIBE_CPP_MODULE_DIR");
    let Some(runtime_dir) = std::env::var_os("DEP_TRANSCRIBE_CPP_RUNTIME_DIR") else {
        return;
    };
    let mut dirs = BTreeSet::new();
    dirs.insert(PathBuf::from(runtime_dir));
    if let Some(module_dir) = std::env::var_os("DEP_TRANSCRIBE_CPP_MODULE_DIR") {
        dirs.insert(PathBuf::from(module_dir));
    }

    let dest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("transcribe-libs");
    // Recreated clean, so a dropped ggml module never lingers in a package.
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).expect("create transcribe-libs staging dir");

    let mut libs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in &dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).flatten() {
            let src = entry.path();
            let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("");
            // By name: Linux versions its libs (libtranscribe.so.0).
            if name.ends_with(".dll") || name.ends_with(".so") || name.contains(".so.") {
                libs.insert(name.to_string(), src);
            }
        }
    }

    // One name per lib: the shortest versioned name (the SONAME) for linked core libs, else the bare `.so` that
    // dlopen'd modules use; `fs::copy` follows the symlink.
    let mut best: BTreeMap<&str, (&str, &PathBuf, usize)> = BTreeMap::new();
    for (name, src) in &libs {
        let (stem, rank) = match split_versioned_so(name) {
            None => (name.as_str(), 0),
            Some((stem, 0)) => (stem, usize::MAX),
            Some((stem, depth)) => (stem, depth - 1),
        };
        match best.get(stem) {
            Some(&(_, _, existing)) if existing <= rank => {}
            _ => {
                best.insert(stem, (name, src, rank));
            }
        }
    }
    let mut dests = vec![dest];
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // OUT_DIR is target/<profile>/build/<crate>-<hash>/out.
        dests.push(PathBuf::from(std::env::var("OUT_DIR").unwrap()).ancestors().nth(3).unwrap().to_path_buf());
    }
    for dest in &dests {
        for &(name, src, _) in best.values() {
            std::fs::copy(src, dest.join(name)).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
        }
    }
    if best.is_empty() {
        panic!("no transcribe-cpp runtime libraries found under {dirs:?}");
    }
    println!("cargo:warning=Staged {} transcribe-cpp runtime library file(s)", best.len());
}

/// `libfoo.so` -> ("libfoo", 0), `libfoo.so.0` -> ("libfoo", 1), `libfoo.so.0.2.0` -> ("libfoo", 3).
fn split_versioned_so(name: &str) -> Option<(&str, usize)> {
    let idx = name.find(".so")?;
    let (stem, rest) = (&name[..idx], &name[idx + 3..]);
    if rest.is_empty() {
        return Some((stem, 0));
    }
    let comps: Vec<&str> = rest.strip_prefix('.')?.split('.').collect();
    comps.iter().all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit())).then_some((stem, comps.len()))
}
