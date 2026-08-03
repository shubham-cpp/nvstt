fn main() {
    if std::env::var_os("CARGO_FEATURE_CUDA_RUNTIME").is_some() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
