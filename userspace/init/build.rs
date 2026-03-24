fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed={dir}/link.x");
    println!("cargo:rustc-link-arg=-T{dir}/link.x");
}
