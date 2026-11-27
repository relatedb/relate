//! The serve banner's one home: every line a serve start can print, as
//! named helpers the prologue composes (render order lives in
//! `kind_banner_lines` and the prologue's comment).

/// The ADR-0306 §5 startup banner line for a non-writable access mode:
/// exactly one line naming the mode. `None` for a writable serve, which
/// adds no banner line. [`super::setup`] fronts the line onto the banner
/// Vec; [`super::run`]'s one banner loop prints it.
pub(super) fn access_banner_line(access: relate::OpenMode) -> Option<&'static str> {
    match access {
        relate::OpenMode::Writable => None,
        relate::OpenMode::ReadOnly => Some(
            "serve: opened the store read-only — no writer lease and \
             never creates the store; every write fails",
        ),
        relate::OpenMode::Follow => Some(
            "serve: opened the store read-only and following the WAL — \
             the writer's committed frames appear as they land",
        ),
        // LABS-903 (ADR-0500): the `--load` memory turn-into — the source
        // loaded into the in-memory store, released after the load.
        relate::OpenMode::Ephemeral => Some(
            "serve: loaded the store into memory — read-only, the source \
             released after the load; every write fails",
        ),
    }
}

/// ADR-0703 §4: a serve open that attached the experimental Vortex
/// property store says so — one line naming the two opt-in spellings,
/// every surface prints it. `None` when no store is attached.
pub(super) fn vortex_store_banner_line(attached: bool) -> Option<String> {
    attached.then(|| format!("serve: {}", crate::session::VORTEX_STORE_NOTE))
}

/// The banner lines every serve kind prints, in render order: the
/// ADR-0306 §5 access-mode line, then the ADR-0703 vortex-store note
/// when a store attached. A new kind-invariant line has this one home —
/// the stdio-only return and the listener prologue both start their
/// banners here, so a stdio-only run announces exactly what a bound run
/// announces (minus the bind-posture lines only a bind can print).
pub(super) fn kind_banner_lines(attached: bool, access: relate::OpenMode) -> Vec<String> {
    let mut banner = Vec::new();
    banner.extend(access_banner_line(access).map(str::to_string));
    banner.extend(vortex_store_banner_line(attached));
    banner
}

/// The loud plaintext-posture warning for a non-loopback bind
/// (ADR-0248): `serve` has no TLS, so binding beyond loopback exposes the
/// surfaces — under the one shared credential — to the network. The
/// loopback verdict and the bind's description are `setup`'s two hoisted
/// bind-posture facts (the caller passes them, never a host to re-derive
/// from). `None` on loopback.
pub(super) fn non_loopback_warning(bind_loopback: bool, bind_description: &str) -> Option<String> {
    (!bind_loopback).then(|| {
        format!(
            "serve: WARNING: {bind_description} exposes the PLAINTEXT HTTP/MCP/Bolt surfaces to the network — \
             serve has no TLS; anyone who can reach the ports can present credentials. \
             Keep 127.0.0.1 or front the server with TLS."
        )
    })
}
