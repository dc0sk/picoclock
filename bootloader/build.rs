use std::{env, fs::File, io::Write, path::PathBuf};

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let (rp2040, rp235x) = (env::var_os("CARGO_FEATURE_RP2040").is_some(), env::var_os("CARGO_FEATURE_RP235X").is_some());
    let memory: &[u8] = match (rp2040, rp235x) {
        (true, false) => include_bytes!("memory-rp2040.x"),
        (false, true) => include_bytes!("memory-rp235x.x"),
        _ => panic!("enable exactly one of the features `rp2040` (Pico W) or `rp235x` (Pico 2 W)"),
    };
    File::create(out.join("memory.x")).unwrap().write_all(memory).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory-rp2040.x");
    println!("cargo:rerun-if-changed=memory-rp235x.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    if rp2040 {
        println!("cargo:rustc-link-arg-bins=-Tlink-rp.x"); // second-stage bootloader
    }
}
