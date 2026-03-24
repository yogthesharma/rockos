//! Builds `userspace/init` and refreshes `assets/init.elf` before the kernel `include_bytes!` it.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=userspace/init/src");
    println!("cargo:rerun-if-changed=userspace/init/link.x");
    println!("cargo:rerun-if-changed=userspace/init/Cargo.toml");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "rockos-init",
            "--release",
            "--target",
            "x86_64-unknown-none",
        ])
        .current_dir(&manifest_dir)
        .status()
        .expect("spawn `cargo build -p rockos-init`");
    if !status.success() {
        panic!("rockos-init (userspace/init) failed to build");
    }

    let elf = manifest_dir.join("target/x86_64-unknown-none/release/init");
    let dst = manifest_dir.join("assets/init.elf");
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(&elf, &dst).expect("copy init ELF to assets/");
}
