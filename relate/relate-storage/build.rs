// `parquet_codec` & co.: the snapshot codecs compiled in (ADR-0105).
// `arrow`/`parquet` are optional and never build on wasm32; the
// experimental codecs are native-only for the same reason.
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "wasm32");
    let mut manifest_codec = false;
    for (feature, cfg) in [
        ("PARQUET", "parquet_codec"),
        ("RKYV_CODEC", "rkyv_codec"),
        ("FJALL_CODEC", "fjall_codec"),
    ] {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() && !wasm {
            println!("cargo::rustc-cfg={cfg}");
            manifest_codec = true;
        }
    }
    // One derived cfg for "some manifest-bearing codec is compiled", so the
    // manifest version facts and the Subsystem::Manifest variant gate on a
    // single name instead of re-spelling the feature disjunction per site.
    println!("cargo::rustc-check-cfg=cfg(manifest_codec)");
    if manifest_codec {
        println!("cargo::rustc-cfg=manifest_codec");
    }
}
