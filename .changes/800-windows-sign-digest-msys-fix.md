### Notes

- Release tooling fix: the Windows signing preflight's smctl digest
  check no longer refuses a matching digest on Git-Bash runners —
  MSYS-mangled `sha256sum` output carried a leading backslash, so the
  v1.3.0 release run's windows leg failed before the credentials
  healthcheck could run. The digest is now read as the first 64 hex
  characters of the stream.
