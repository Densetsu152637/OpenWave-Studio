//! Builds the fixture plugins as shared libraries and writes them as plugin bundles.
//!
//! Cargo cannot copy a crate's own `cdylib` anywhere once it is built, and the plugins have to
//! be real shared libraries with a conventional name in a known folder. So this script runs a
//! nested `cargo rustc --crate-type cdylib` of this very package, once per fixture (selected by
//! a Cargo feature), into a private target folder, and copies the results to
//! `<target>/plugin-fixtures/` (release) or `<target>/plugin-fixtures-debug/` (other profiles).
//! A CLAP result is a `.clap` bundle folder on macOS and the library file renamed `.clap`
//! elsewhere. The VST3 result follows the VST3 module layout: `Contents/MacOS/<name>` plus an
//! `Info.plist` on macOS, `Contents/<arch>-linux/<name>.so` on Linux, and the plain library renamed
//! `.vst3` on Windows. A nested build is flagged with `OW_FIXTURES_NESTED` so it does not
//! recurse.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// (Cargo feature, bundle name without the extension, extension) of every fixture library.
const FIXTURES: &[(&str, &str, &str)] = &[
    ("gain", "ow-fixture-gain", "clap"),
    ("crash", "ow-fixture-crash", "clap"),
    ("vst3", "ow-fixture-gain", "vst3"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=OW_FIXTURES_NESTED");

    let release = env::var("PROFILE").is_ok_and(|profile| profile == "release");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = target_dir(&out_dir);
    let bundles = target.join(if release {
        "plugin-fixtures"
    } else {
        "plugin-fixtures-debug"
    });
    println!("cargo:rustc-env=OW_FIXTURE_DIR={}", bundles.display());
    if env::var_os("OW_FIXTURES_NESTED").is_some() {
        return;
    }

    fs::create_dir_all(&bundles).expect("create the fixture folder");
    let nested_target = target.join("plugin-fixtures-build");
    for (feature, name, extension) in FIXTURES {
        let library = build_cdylib(feature, release, &nested_target);
        install(&library, &bundles, name, extension);
    }
}

/// The Cargo target folder this build writes under (`<target>` or `<target>/<triple>`).
fn target_dir(out_dir: &Path) -> PathBuf {
    if let Some(dir) = env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    // OUT_DIR is `<target>/<profile>/build/<package>-<hash>/out`.
    out_dir
        .ancestors()
        .nth(4)
        .expect("OUT_DIR is inside the target folder")
        .to_path_buf()
}

/// Builds this package's library as a shared library with `feature`; returns the file.
fn build_cdylib(feature: &str, release: bool, target: &Path) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest =
        Path::new(&env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir")).join("Cargo.toml");
    let mut command = Command::new(cargo);
    command
        .arg("rustc")
        .arg("--lib")
        .arg("--crate-type=cdylib")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(target)
        .arg("--features")
        .arg(feature);
    if release {
        command.arg("--release");
    }
    command
        .env("OW_FIXTURES_NESTED", "1")
        // `cargo clippy` sets this; the nested build is a plain build.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    let status = command.status().expect("run cargo for the fixture plugins");
    assert!(status.success(), "building the {feature} fixture failed");
    let profile = if release { "release" } else { "debug" };
    let (prefix, extension) = if cfg!(windows) {
        ("", "dll")
    } else if cfg!(target_os = "macos") {
        ("lib", "dylib")
    } else {
        ("lib", "so")
    };
    target
        .join(profile)
        .join(format!("{prefix}ow_plugin_fixtures.{extension}"))
}

/// Writes `library` as the module `<bundles>/<name>.<extension>`, unless it already holds the
/// same bytes (a loaded library cannot be replaced on Windows).
fn install(library: &Path, bundles: &Path, name: &str, extension: &str) {
    let bytes = fs::read(library).expect("read the built fixture");
    if extension == "vst3" && !cfg!(any(windows, target_os = "macos")) {
        // The VST3 module layout on Linux: a bundle folder with the library in `Contents/<arch>-linux`.
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64-linux"
        } else {
            "x86_64-linux"
        };
        let dir = bundles
            .join(format!("{name}.vst3"))
            .join("Contents")
            .join(arch);
        fs::create_dir_all(&dir).expect("create the bundle");
        write_if_changed(&dir.join(format!("{name}.so")), &bytes);
    } else if cfg!(target_os = "macos") {
        let contents = bundles.join(format!("{name}.{extension}")).join("Contents");
        let binary = contents.join("MacOS").join(name);
        fs::create_dir_all(binary.parent().expect("parent")).expect("create the bundle");
        write_if_changed(&binary, &bytes);
        let info = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>CFBundleExecutable</key>\n\t<string>{name}</string>\n\
             \t<key>CFBundleIdentifier</key>\n\t<string>com.openwave.fixture.{name}.{extension}</string>\n\
             \t<key>CFBundleName</key>\n\t<string>{name}</string>\n\
             \t<key>CFBundlePackageType</key>\n\t<string>BNDL</string>\n\
             \t<key>CFBundleSignature</key>\n\t<string>????</string>\n\
             \t<key>CFBundleVersion</key>\n\t<string>1.0.0</string>\n\
             </dict>\n</plist>\n"
        );
        write_if_changed(&contents.join("Info.plist"), info.as_bytes());
        write_if_changed(&contents.join("PkgInfo"), b"BNDL????");
    } else {
        write_if_changed(&bundles.join(format!("{name}.{extension}")), &bytes);
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return;
    }
    if let Err(error) = fs::write(path, bytes) {
        println!(
            "cargo:warning=could not update {} ({error}); it may be loaded by a running process",
            path.display()
        );
    }
}
