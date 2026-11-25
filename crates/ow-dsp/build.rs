//! Compiles the vendored Signalsmith Stretch (header-only C++) behind the small C wrapper in
//! `native/`. Needs a C++ compiler only; no libclang or bindgen.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=vendor/signalsmith-stretch");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("native/ow_stretch.cpp")
        .include("native")
        .include("vendor/signalsmith-stretch")
        .std("c++14")
        .warnings(false);
    if build.get_compiler().is_like_msvc() {
        build.flag("/EHsc");
    }
    build.compile("ow_signalsmith_stretch");
}
