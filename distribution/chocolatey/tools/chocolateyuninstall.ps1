$ErrorActionPreference = 'Stop'

# Chocolatey removes the package directory (the binary and its
# .relate-package-managed marker) itself; only the shim needs removing.
Uninstall-BinFile -Name 'relate'
