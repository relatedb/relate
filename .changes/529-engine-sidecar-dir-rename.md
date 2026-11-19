### Changed

- The engine's sidecar directory inside a store is `.relate/`, not
  `.cypherlite/` (the schema-files mirror moves with it): a writable
  open adopts an existing store in place with one directory rename,
  run as the last step before the lease so an open refused by any
  earlier check never renames the user's directory — natively, and
  through a caller-supplied `Vfs` (`Database::open_vfs`,
  `StoreTarget::from_vfs_with_codec`) the same way. A read-only open
  never adopts: a store still carrying the legacy `.cypherlite/`
  markdown sidecar refuses closed, naming the one writable pass that
  adopts it, and a legacy `.cypherlite/` without the manifest (a
  classic store's schema mirror alone) is inert and opens unchanged.
  The lock and writer marker files keep their names.
