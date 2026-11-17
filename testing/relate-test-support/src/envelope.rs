//! The first-party children's manifest pins (ADR-0430 §6): a child's
//! serialized request envelope and the committed manifest it serves must
//! name the same keys, and the manifest must declare the mount contract the
//! wire kit defines.

use serde_json::Value;

/// Assert the envelope-keys contract over a serialized envelope example
/// and the committed manifest it must agree with (ADR-0430 §6): the
/// example's keys, sorted, are exactly the manifest's flag names plus
/// its positional's name, sorted. A disagreement panics naming both
/// sides, so a manifest edit that breaks the child's envelope fails in
/// the child's crate, not mid-import.
///
/// Test support, not product code: each child's test serializes its own
/// envelope example and hands it to this one assertion.
pub fn assert_envelope_keys(example: &Value, manifest: &Value) {
    let mut keys: Vec<String> = example
        .as_object()
        .expect("a serialized envelope is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    let mut declared: Vec<String> = manifest["flags"]
        .as_array()
        .expect("the flag table is an array")
        .iter()
        .map(|flag| {
            flag["name"]
                .as_str()
                .expect("a flag entry names itself")
                .to_string()
        })
        .collect();
    declared.push(
        manifest["positional"]["name"]
            .as_str()
            .expect("the positional's name")
            .to_string(),
    );
    declared.sort();
    assert_eq!(
        keys, declared,
        "the manifest's declared names and the envelope's keys must agree"
    );
}

/// Assert the mount-contract half of a child's committed manifest: a
/// `command` extension named `name`, speaking the wire kit's one MCP
/// revision with no retired `wire` selector, committed per batch, taking
/// the `DIR` positional on a store that need not be fresh. The flag table
/// stays the child's own assertion.
pub fn assert_mount_contract(manifest: &Value, name: &str) {
    assert_eq!(manifest["kind"], "command");
    assert_eq!(manifest["name"], name);
    // The protocol names the MCP revision the mount serves, and the
    // retired dual-wire selector is gone: a package declares the one wire
    // ADR-0447 leaves.
    assert_eq!(manifest["protocol"], relate_wire::mcp::MCP_PROTOCOL_VERSION);
    assert!(
        manifest.get("wire").is_none(),
        "the manifest carries no wire field: the mount speaks MCP stdio only"
    );
    assert_eq!(manifest["commit"], relate_wire::PER_BATCH);
    assert_eq!(manifest["positional"]["name"], "DIR");
    assert_eq!(
        manifest["requires-fresh-store"], false,
        "the manifest declares requires-fresh-store: false"
    );
}
