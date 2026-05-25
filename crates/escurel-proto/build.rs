//! Compile the v1 .proto into Rust via tonic-build. Output lands
//! in `OUT_DIR` and is `include!`d from `lib.rs`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "proto/escurel.proto";
    println!("cargo:rerun-if-changed={proto}");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto], &["proto"])?;
    Ok(())
}
