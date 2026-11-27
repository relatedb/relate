// `parquet_codec` & `rkyv_codec`: the snapshot codecs compiled in
// (ADR-0105). `arrow`/`parquet` are optional and never build on wasm32; the
// rkyv codec is native-only for the same reason. The Vortex codec's storage
// half compiles unconditionally: its shard serialization arrives from the
// experiment crate at runtime, so it needs no cfg (ADR-0677).
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "wasm32");
    for (feature, cfg) in [("PARQUET", "parquet_codec"), ("RKYV_CODEC", "rkyv_codec")] {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() && !wasm {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
}
