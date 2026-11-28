fn main() {
    // The runner executes inside the upstream Kanidm server image, which is a scratch image
    // containing only kanidmd's runtime libraries. Keep the runner self-contained instead of
    // depending on the libc or loader of either the Kaniop or Kanidm image.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-static");
    }
}
