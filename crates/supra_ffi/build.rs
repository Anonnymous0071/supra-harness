//! Build script: compile the C++20 libraries and link them statically.
//!
//! Two build trees exist by design, and the distinction is worth stating because
//! it looks like duplication:
//!
//!   * `just build-cpp` configures `build/` and is what `CTest` runs against. It
//!     builds the C++ test executables, which Cargo has no reason to.
//!   * This script configures a tree under `OUT_DIR`. That is the only form that
//!     works for a downstream `cargo build` with no `just` available, honours the
//!     Cargo profile, and keeps parallel target directories from colliding.
//!
//! Set `SUPRA_CPP_BUILD_DIR` to an existing configured tree to reuse it and skip
//! the second build. Useful in a tight local loop; not used in CI, where the
//! independent build is the point.

// A build script's only failure channel is to abort: Cargo renders the message as
// a build error, which is precisely the right outcome when a workspace assumption
// is broken. The workspace-wide ban on `expect` exists to stop a long-running
// agent process from dying mid-session, and that rationale does not reach a
// script that runs once before any session exists - the `assert!` calls below
// already rely on the same mechanism, and clippy does not flag those.
#![allow(clippy::expect_used)]

use std::env;
use std::path::{Path, PathBuf};

// Shared with src/sys.rs. The same constants are asserted against Rust's
// `size_of` there and against C++ `sizeof` in abi_check.cpp, so a layout
// divergence cannot compile from either direction.
include!("abi_sizes.rs");

/// The three libraries, in dependency order: `libsupra_ansi` links `libsupra_width`.
const LIBRARIES: [&str; 3] = ["supra_width", "supra_ansi", "supra_sandbox"];

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR is unset; this file only runs as a Cargo build script"),
    );
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate must live at <workspace-root>/crates/<name>")
        .to_path_buf();

    let cpp_root = workspace_root.join("cpp");
    assert!(cpp_root.is_dir(), "cpp/ not found at {}; the workspace layout changed", cpp_root.display());

    emit_rerun_directives(&workspace_root, &manifest_dir);

    let lib_dir = match env::var_os("SUPRA_CPP_BUILD_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir).join("lib");
            assert!(
                dir.is_dir(),
                "SUPRA_CPP_BUILD_DIR is set but {} does not exist; run `just build-cpp` first",
                dir.display()
            );
            dir
        }
        None => build_with_cmake(&workspace_root),
    };

    for library in LIBRARIES {
        let archive = lib_dir.join(format!("lib{library}.a"));
        assert!(archive.is_file(), "expected archive {} was not produced", archive.display());
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Reverse dependency order: a static linker resolves left to right, so a
    // library must appear before the one it depends on. libsupra_ansi calls into
    // libsupra_width, so width must come after ansi.
    for library in LIBRARIES.iter().rev() {
        println!("cargo:rustc-link-lib=static={library}");
    }

    link_cxx_runtime();
    compile_abi_check(&cpp_root, &manifest_dir);
}

/// Rebuild when any C++ input changes.
///
/// Enumerated rather than left to Cargo's default, which watches only the crate
/// directory - so an edit to a header two directories up would not trigger a
/// rebuild, and the Rust side would link a stale archive.
fn emit_rerun_directives(workspace_root: &Path, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed={}", manifest_dir.join("build.rs").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("abi_sizes.rs").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("abi_check.cpp").display());
    println!("cargo:rerun-if-changed={}", workspace_root.join("CMakeLists.txt").display());
    println!("cargo:rerun-if-changed={}", workspace_root.join("cmake").display());
    println!("cargo:rerun-if-changed={}", workspace_root.join("cpp").display());
    println!("cargo:rerun-if-env-changed=SUPRA_CPP_BUILD_DIR");
    println!("cargo:rerun-if-env-changed=SUPRA_CXX");
}

fn build_with_cmake(workspace_root: &Path) -> PathBuf {
    let mut config = cmake::Config::new(workspace_root);

    // The C++ test executables are CTest's concern, not Cargo's. Skipping them
    // roughly halves this build.
    config.define("SUPRA_BUILD_TESTS", "OFF");

    // clang is the pinned compiler for this tree, matching `just build-cpp`, so
    // the archives Cargo links are built by the same frontend that the C++ suites
    // were verified with. Overridable for a deliberate experiment.
    if let Some(cxx) = env::var_os("SUPRA_CXX") {
        config.define("CMAKE_CXX_COMPILER", cxx);
    } else {
        config.define("CMAKE_CXX_COMPILER", "clang++");
    }

    // `cmake --build --target all` would also build nothing extra now that tests
    // are off, but naming the targets keeps the intent explicit.
    config.build_target("all");

    let dst = config.build();
    dst.join("build").join("lib")
}

/// Link the C++ standard library.
///
/// Required because the archives contain C++ code even though the ABI is C:
/// `std::snprintf`, `std::memcpy`, and the internal `std::string`-free helpers
/// still pull in libstdc++ or libc++ symbols. Rust does not link it by default.
fn link_cxx_runtime() {
    let target = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env_abi = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    match target.as_str() {
        "linux" | "android" => {
            // Which runtime depends on the compiler, not the OS. clang defaults
            // to libstdc++ on Linux distributions, and mismatching it produces
            // undefined symbols at link time rather than anything subtler.
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        "macos" | "ios" => {
            println!("cargo:rustc-link-lib=dylib=c++");
        }
        "windows" if env_abi == "gnu" => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        // MSVC links its runtime automatically.
        _ => {}
    }
}

/// Compile the ABI assertion translation unit.
///
/// Nothing calls into it. Its only purpose is to fail the build when a C++
/// `sizeof` disagrees with `abi_sizes.rs`, which is what stops a hand-written
/// extern declaration from silently drifting from its header.
fn compile_abi_check(cpp_root: &Path, manifest_dir: &Path) {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .file(manifest_dir.join("abi_check.cpp"))
        .include(cpp_root.join("libsupra_width").join("include"))
        .include(cpp_root.join("libsupra_ansi").join("include"))
        .include(cpp_root.join("libsupra_sandbox").join("include"))
        .warnings(true)
        .flag_if_supported("-Werror");

    for (name, value) in [
        ("SIZEOF_ANSI_COLOR", SIZEOF_ANSI_COLOR),
        ("ALIGNOF_ANSI_COLOR", ALIGNOF_ANSI_COLOR),
        ("SIZEOF_ANSI_STYLE", SIZEOF_ANSI_STYLE),
        ("ALIGNOF_ANSI_STYLE", ALIGNOF_ANSI_STYLE),
        ("SIZEOF_ANSI_TOKEN", SIZEOF_ANSI_TOKEN),
        ("ALIGNOF_ANSI_TOKEN", ALIGNOF_ANSI_TOKEN),
        ("SIZEOF_ANSI_SCANNER", SIZEOF_ANSI_SCANNER),
        ("ALIGNOF_ANSI_SCANNER", ALIGNOF_ANSI_SCANNER),
        ("SIZEOF_ANSI_TRUNCATION", SIZEOF_ANSI_TRUNCATION),
        ("ALIGNOF_ANSI_TRUNCATION", ALIGNOF_ANSI_TRUNCATION),
        ("SIZEOF_SANDBOX_CAPABILITIES", SIZEOF_SANDBOX_CAPABILITIES),
        ("ALIGNOF_SANDBOX_CAPABILITIES", ALIGNOF_SANDBOX_CAPABILITIES),
        ("SIZEOF_SANDBOX_PATH_RULE", SIZEOF_SANDBOX_PATH_RULE),
        ("ALIGNOF_SANDBOX_PATH_RULE", ALIGNOF_SANDBOX_PATH_RULE),
        ("SIZEOF_SANDBOX_POLICY", SIZEOF_SANDBOX_POLICY),
        ("ALIGNOF_SANDBOX_POLICY", ALIGNOF_SANDBOX_POLICY),
        ("SIZEOF_SANDBOX_PROCESS", SIZEOF_SANDBOX_PROCESS),
        ("ALIGNOF_SANDBOX_PROCESS", ALIGNOF_SANDBOX_PROCESS),
        ("SIZEOF_SANDBOX_COMMAND", SIZEOF_SANDBOX_COMMAND),
        ("ALIGNOF_SANDBOX_COMMAND", ALIGNOF_SANDBOX_COMMAND),
    ] {
        build.define(&format!("SUPRA_ABI_{name}"), value.to_string().as_str());
    }

    build.compile("supra_ffi_abi_check");
}
