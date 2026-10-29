fn main() {
    // Only the linker flag below depends on this script. Without an explicit
    // input Cargo reruns it for README/report changes across the whole tree.
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Windows reserves only 1 MiB for the executable's main thread by
        // default. Framework rendering enters Rust DOM callbacks from JS;
        // give QuickJS's bounded stack and those callbacks enough headroom.
        println!("cargo:rustc-link-arg=/STACK:8388608");
    }
}
