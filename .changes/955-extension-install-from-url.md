### Added

- `relate extension install <name> --from URL` installs a package from a URL: the
  manifest the URL names (a last path segment ending in `.json`), or a path
  `extension.json` is appended to, with the executable fetched from the manifest's own
  folder or a relative subfolder. A pasted `github.com` blob or tree URL is rewritten to
  its raw-content form first, so the page you browsed installs as-is.

### Fixed

- A `--github` install records its pin before the package lands and rolls it back when
  the landing fails, so a failed install can no longer leave an installed package whose
  recorded pin is missing (which would have let a later install approve a moved pin
  silently).
