//! The `vortex-store` gate's public contract: the install hook reaches the
//! shard-codec registry, so the Vortex codec resolves as compiled after it
//! runs. In its own test binary, so the process-wide install stays out of
//! every other suite's compiled-codec set.

#![cfg(feature = "vortex-store")]

use relate_storage::{install_vortex_shard_codec, SnapshotCodecKind};

#[test]
fn the_install_hook_publishes_the_codec() {
    assert!(!SnapshotCodecKind::Vortex.is_compiled());
    install_vortex_shard_codec();
    assert!(SnapshotCodecKind::Vortex.is_compiled());
}
