### Notes

- The wasm bundle's npm manifest and the files its build produces are
  now guarded: `cargo xtask check-boundaries` fails when a `js/` helper
  is missing from `package.json.template`'s `files` array, a `files`
  entry names no file the build produces, or an `exports` subpath points
  outside `files`; the build copies the whole `js/` directory so there is
  one inventory to keep in step.
- The Go client's standalone recipe (one link-time variable,
  `CGO_LDFLAGS="-L<dir>"`, with the module's own header and
  `-lrelate_ffi`) is documented in `clients/go/README.md` and
  `docs/api/go/spec.md`, and proven by `scripts/check-go-standalone.sh`
  in `make test-go`.
