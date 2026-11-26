### Notes

- CodeQL runs as an advanced-setup workflow whose Swift job builds the
  XCFramework from the checkout, so a Swift API added between releases no
  longer fails code scanning against the last released binary.
