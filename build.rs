fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Windows reserves only 1 MiB for the executable's main thread by
        // default. Framework rendering enters Rust DOM callbacks from JS;
        // give QuickJS's bounded stack and those callbacks enough headroom.
        println!("cargo:rustc-link-arg=/STACK:8388608");
    }
}
