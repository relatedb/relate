//! The concurrent vault read's tests (the striped pass `walk_vault`
//! runs over large vaults): the striped result is exactly the serial
//! pass's — same files, same order — and a failing stripe reports the
//! first failure in walk order.

use crate::codec::markdown::vault::read::{take_read_workers, with_read_workers};
use crate::codec::markdown::vault::walk_vault;
use crate::codec::SnapshotCodecKind;

/// A memory-store vault of `count` labeled files, each with its index
/// in both the path and the body, so the read's result is fully
/// checkable.
fn striped_vault(count: usize) -> crate::StoreTarget {
    let store = crate::memory_store_with_codec(SnapshotCodecKind::Markdown).unwrap();
    let vfs = store.vfs();
    for index in 0..count {
        let label = if index % 2 == 0 { "Even" } else { "Odd" };
        let path = format!("{label}/note-{index:03}.md");
        let text = format!("---\ntype: {label}\nname: note-{index:03}\n---\nbody {index}\n");
        crate::vfs::atomic_write(&*vfs, &path, text.as_bytes()).unwrap();
    }
    store
}

/// The walk's answer as comparable pairs — path and bytes, in walk
/// order.
fn walked_pairs(store: &crate::StoreTarget) -> Vec<(String, Vec<u8>)> {
    walk_vault(&*store.vfs())
        .unwrap()
        .into_iter()
        .map(|file| (file.walked.path, file.bytes))
        .collect()
}

#[test]
fn the_striped_read_returns_the_serial_passs_files_in_order() {
    // Forty files is four stripes at the forced count of four (and
    // three at the natural stripe size), so the parallel path actually
    // splits the walk.
    let store = striped_vault(40);
    let serial = with_read_workers(1, || walked_pairs(&store));
    let striped = with_read_workers(4, || {
        assert_eq!(take_read_workers(), 0, "the counter starts clean");
        walked_pairs(&store)
    });
    assert_eq!(take_read_workers(), 4, "the striped pass ran four workers");
    assert_eq!(serial, striped);
    assert_eq!(striped.len(), 40);
    assert!(
        striped.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "the striped fold keeps the walk's global path order"
    );
}

#[test]
fn a_small_vault_reads_serially_without_a_spawn() {
    // Fewer files than one READ_STRIPE: a single stripe, so the
    // natural policy reads serially and the striped path never runs.
    let store = striped_vault(15);
    let files = walked_pairs(&store);
    assert_eq!(files.len(), 15);
    assert_eq!(take_read_workers(), 0, "the serial path spawns nothing");
}

#[cfg(unix)]
#[test]
fn the_striped_read_fails_like_the_serial_pass() {
    // An unreadable file mid-vault, with the striped path forced on:
    // the fold must surface the stripe's read failure — the same
    // failure the serial pass returns for the same vault, so the
    // striped pass never swallows an I/O error behind a join.
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("Person")).unwrap();
    for name in ["Ada", "Grace", "Hopper"] {
        std::fs::write(
            vault.join("Person").join(format!("{name}.md")),
            format!("---\ntype: Person\nname: {name}\n---\n"),
        )
        .unwrap();
    }
    let store = crate::parse_uri_with_options(
        vault.to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let deny = std::fs::Permissions::from_mode(0o000);
    std::fs::set_permissions(vault.join("Person/Grace.md"), deny).unwrap();
    let read_or_die = |workers: usize| {
        with_read_workers(workers, || match walk_vault(&*store.vfs()) {
            Ok(files) => panic!(
                "the unreadable vault must fail the read, got {} files",
                files.len()
            ),
            Err(e) => e,
        })
    };
    let serial = read_or_die(1);
    assert_eq!(take_read_workers(), 0);
    let striped = read_or_die(2);
    assert_eq!(take_read_workers(), 2, "the striped path actually ran");
    assert_eq!(striped.to_string(), serial.to_string());
    assert!(striped.to_string().contains("Permission denied"));
}
