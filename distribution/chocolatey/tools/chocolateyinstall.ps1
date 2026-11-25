$ErrorActionPreference = 'Stop'

# The package's tools directory: where the real binary and its provenance
# marker live. $MyInvocation resolves to this script only at file scope, so
# the constant is defined here, not inside a function.
$RelateToolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

# The one asset this package ships: the Authenticode-signed Windows x86-64
# CLI (ADR-0594). The package version is the release tag without its `v`,
# so the template serves every release.
$RelateAsset = 'relate-windows-amd64.exe'
$RelateReleaseUrl = 'https://github.com/relatedb/relate/releases/download'

# Install provenance (ADR-0659): the marker beside the real binary - never
# the shim - so `relate upgrade` refuses to self-replace and names
# `choco upgrade relate` instead.
$RelatePackageMarker = '.relate-package-managed'

# The pinned release signing public key (ADR-0115), byte-identical to
# install.sh's and install.ps1's (the install harness's drift guard holds
# the copies together). A Chocolatey package cannot source install.ps1 - it
# is installed from the community repository - so this template carries its
# own copy of the signature verification, and the checksum it gates the
# download on comes from the signed set, never from the release manifest.
$RelateSigningKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHgv/GHY5mtv/HopH/J/T7KrTT7eOVFEG2ueT4iRKwbK relate-release'

# The signer identity in allowed_signers and the SSH signature namespace:
# one value, `relate-release`, at both sites (ADR-0115).
$RelateSigner = 'relate-release'

# The ADR-0457 signed-manifest version header, byte-identical to
# install.ps1's and install.sh's (one space after the colon is load-bearing).
$RelateSumsVersionPrefix = '# relate-release-version: '

# The last release published before the signed version header existed
# (ADR-0457): a headerless manifest is accepted only at or below it.
$RelateLegacyManifestCeiling = '0.14.0'

# The release-tag grammar: a release tag is vX.Y.Z and nothing else. \A and
# \z, not ^ and $: .NET's $ also matches before a final newline.
$RelateTagPattern = '\Av[0-9]+\.[0-9]+\.[0-9]+\z'

# The Authenticode signer certificate the package pins (ADR-0594): the
# release pipeline's pack step writes the thumbprint the windows-sign action
# pins into this file beside the script. The check refuses a Valid signature
# from any other certificate, and refuses to install when the file is
# missing or empty, so a package that carries no pin installs nothing.
$RelateThumbprintFile = Join-Path $RelateToolsDir 'signer-thumbprint.txt'

function Stop-RelateChocolateyInstall {
    param([string] $Message)
    throw "chocolateyinstall.ps1: $Message"
}

# The release tag the package version names. It must be vX.Y.Z: anything
# else would derive a release URL that no release has.
function Get-RelateReleaseTag {
    param([string] $Version)
    $tag = "v$Version"
    if ($tag -cnotmatch $RelateTagPattern) {
        Stop-RelateChocolateyInstall ("the package version '$Version' is not X.Y.Z " +
            "(the release tag '$tag' is not a vX.Y.Z tag); installing nothing")
    }
    return $tag
}

# True when the X.Y.Z version $A is greater than $B. Unparseable input counts
# as greater, so the caller takes the fail-closed branch (install.sh's
# version_gt). Components compare as digit strings, so no width overflows.
function Test-RelateVersionGreater {
    param([string] $A, [string] $B)
    $pa = $A.Split('.')
    $pb = $B.Split('.')
    if ($pa.Count -ne 3 -or $pb.Count -ne 3) { return $true }
    for ($i = 0; $i -lt 3; $i++) {
        if ($pa[$i] -cnotmatch '\A[0-9]+\z' -or $pb[$i] -cnotmatch '\A[0-9]+\z') { return $true }
        $x = $pa[$i].TrimStart('0')
        $y = $pb[$i].TrimStart('0')
        if ($x.Length -ne $y.Length) { return ($x.Length -gt $y.Length) }
        $order = [string]::CompareOrdinal($x, $y)
        if ($order -ne 0) { return ($order -gt 0) }
    }
    return $false
}

# Bind an authenticated SHA256SUMS to the release the metadata named
# (ADR-0457): the version header is inside the signed bytes, so a validly
# signed older manifest cannot be replayed for a newer tag. A present but
# empty header is a publish bug and fails hard; a headerless manifest is
# accepted, with a warning, only for a tag at or below the legacy ceiling.
function Assert-RelateManifestVersion {
    param([string] $Sums, [string] $Tag)
    $firstLine = ($Sums -split "`n")[0]
    if ($firstLine.StartsWith($RelateSumsVersionPrefix, [StringComparison]::Ordinal)) {
        $sumsVersion = $firstLine.Substring($RelateSumsVersionPrefix.Length).Trim()
        if ($sumsVersion -eq '') {
            Stop-RelateChocolateyInstall "SHA256SUMS from $Tag carries an empty version header; installing nothing"
        }
        if ($sumsVersion -cne $Tag) {
            Stop-RelateChocolateyInstall "SHA256SUMS is for $sumsVersion, not the requested release $Tag; installing nothing"
        }
    } elseif (Test-RelateVersionGreater ($Tag -replace '^v', '') $RelateLegacyManifestCeiling) {
        Stop-RelateChocolateyInstall ("SHA256SUMS from $Tag has no signed version header; refusing to install a manifest " +
            "whose version is not authenticated (legacy manifests are accepted only at or below " +
            "v$RelateLegacyManifestCeiling); installing nothing")
    } else {
        Write-Warning ("chocolateyinstall.ps1: SHA256SUMS from $Tag has no signed version header (legacy manifest, " +
            "accepted only at or below v$RelateLegacyManifestCeiling)")
    }
}

# $Child under the Windows System32 directory, or $null when SystemRoot is
# not set: the one derivation of the inbox location both callers use.
function Get-RelateSystem32Path {
    param([string] $Child)
    if (-not $env:SystemRoot) { return $null }
    return (Join-Path (Join-Path $env:SystemRoot 'System32') $Child)
}

# ssh-keygen: the inbox Windows OpenSSH client first (present by default
# since Windows 10 1809), PATH only when it is absent, or a refusal naming
# the Add-WindowsCapability command. The verifier decides whether a checksum
# set is trusted, so the system copy wins over whatever a PATH entry earlier
# in the search order would resolve.
function Resolve-RelateSshKeygen {
    $inbox = Get-RelateSystem32Path 'OpenSSH\ssh-keygen.exe'
    if ($inbox -and (Test-Path -LiteralPath $inbox)) { return $inbox }
    $command = Get-Command ssh-keygen -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($command) { return $command.Path }
    Stop-RelateChocolateyInstall ('ssh-keygen (the Windows OpenSSH client) is required to verify release signatures. ' +
        'Install it from an elevated PowerShell with: Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0 ' +
        '- then retry.')
}

# Run $FilePath with the command line $Arguments exactly as given and return
# its exit code; stdout and stderr go straight to the console. The one
# process launcher: callers build the command line themselves, never through
# PowerShell's native-argument passing, which differs between 5.1 and 7.
function Invoke-RelateProcess {
    param([string] $FilePath, [string] $Arguments)
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $FilePath
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $process = [System.Diagnostics.Process]::Start($info)
    try {
        $process.WaitForExit()
        return $process.ExitCode
    } finally {
        $process.Dispose()
    }
}

# Verify the SSHSIG $SigPath over the checksum set $SumsPath against the
# pinned key (ADR-0115), or fail with the authored refusal for $Tag. The
# allowed_signers file is written into $WorkDir from $RelateSigner and
# $RelateSigningKey, never fetched - the same trust step install.ps1 runs.
# `ssh-keygen -Y verify` reads the signed message only from stdin, and not
# every Windows build receives it intact from a .NET stdin pipe, so cmd.exe's
# `<` hands it the file handle and the exact bytes are verified.
function Assert-RelateManifestSignature {
    param([string] $Keygen, [string] $WorkDir, [string] $SigPath, [string] $SumsPath, [string] $Tag)
    $allowedSigners = Join-Path $WorkDir 'allowed_signers'
    foreach ($path in @($Keygen, $allowedSigners, $SigPath, $SumsPath)) {
        if ($path -match '["%]') {
            Stop-RelateChocolateyInstall "cannot verify through a path holding a quote or a percent sign: $path"
        }
    }
    [IO.File]::WriteAllText($allowedSigners, "$RelateSigner $RelateSigningKey`n", (New-Object System.Text.UTF8Encoding $false))
    $shell = $env:ComSpec
    if (-not $shell) { $shell = Get-RelateSystem32Path 'cmd.exe' }
    if (-not $shell) {
        Stop-RelateChocolateyInstall 'cannot locate cmd.exe to verify the signature: neither ComSpec nor SystemRoot is set'
    }
    $template = '"{0}" -Y verify -f "{1}" -I {2} -n {2} -s "{3}" < "{4}"'
    $verify = $template -f $Keygen, $allowedSigners, $RelateSigner, $SigPath, $SumsPath
    # /s strips the outer quote pair and runs the rest as written.
    if ((Invoke-RelateProcess -FilePath $shell -Arguments ('/d /s /c "' + $verify + '"')) -ne 0) {
        Stop-RelateChocolateyInstall "SHA256SUMS signature verification failed for $Tag; installing nothing"
    }
}

# The expected SHA-256 for $Asset from an authenticated SHA256SUMS. Lines are
# "<64-hex>  <name>"; the first entry naming the asset is the one checked,
# and a missing entry fails.
function Get-RelateSumsEntry {
    param([string] $Sums, [string] $Asset, [string] $Tag)
    $entryPattern = '^([0-9a-fA-F]{64})[ \t]+\*?' + [regex]::Escape($Asset) + '$'
    foreach ($line in ($Sums -split "`n")) {
        $match = [regex]::Match($line.TrimEnd("`r"), $entryPattern)
        if ($match.Success) { return $match.Groups[1].Value }
    }
    Stop-RelateChocolateyInstall "SHA256SUMS from $Tag has no entry for $Asset"
}

# A file's SHA-256, uppercase hex — through .NET, not Get-FileHash:
# Chocolatey's PowerShell host does not reliably load the Utility
# module on every host version ("Get-FileHash is not recognized", seen
# on the v1.3.0 runner and surviving an explicit Import-Module), while
# the .NET crypto classes the rest of this script already leans on are
# always present.
function Get-RelateFileSha256 {
    param([string] $Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($Path)
        try {
            return ([System.BitConverter]::ToString($sha256.ComputeHash($stream))).Replace('-', '')
        } finally {
            $stream.Close()
        }
    } finally {
        $sha256.Dispose()
    }
}

# The pinned Authenticode signer thumbprint, normalized to the form Windows
# reports (uppercase hex without spaces), or a refusal when the package
# carries no pin - a package that pins nothing installs nothing.
function Get-RelatePinnedThumbprint {
    $thumbprint = $null
    if (Test-Path -LiteralPath $RelateThumbprintFile) {
        $thumbprint = ([IO.File]::ReadAllText($RelateThumbprintFile)).Trim()
    }
    if (-not $thumbprint) {
        Stop-RelateChocolateyInstall ("the package pins no Authenticode signer thumbprint " +
            "($RelateThumbprintFile is missing or empty); installing nothing")
    }
    return ($thumbprint -replace ' ', '').ToUpperInvariant()
}

# The downloaded binary must carry a valid Authenticode signature (ADR-0594)
# and be signed by the certificate the package pins - a Valid signature from
# any other certificate is refused, as is a package with no pin.
function Assert-RelateAuthenticodeSignature {
    param([string] $Path)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid') {
        Stop-RelateChocolateyInstall ("$RelateAsset has no valid Authenticode signature " +
            "(status: $($signature.Status), $($signature.StatusMessage)); installing nothing")
    }
    $expected = Get-RelatePinnedThumbprint
    $actual = ($signature.SignerCertificate.Thumbprint -replace ' ', '').ToUpperInvariant()
    if ($actual -ne $expected) {
        Stop-RelateChocolateyInstall ("$RelateAsset is signed by certificate $actual, " +
            "not the pinned signer $expected; installing nothing")
    }
}

function Install-RelateChocolatey {
    $tag = Get-RelateReleaseTag $env:ChocolateyPackageVersion
    $releaseUrl = "$RelateReleaseUrl/$tag"
    $exePath = Join-Path $RelateToolsDir 'relate.exe'

    $keygen = Resolve-RelateSshKeygen

    # One fresh staging directory per install, under %TEMP%: the signed
    # checksum set and the binary are downloaded there, never to a path a
    # stale file from an earlier install could satisfy, and the directory is
    # removed once the verified binary has moved into the package's tools
    # directory.
    $staging = Join-Path $env:TEMP ('relate-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $staging | Out-Null
    try {
        # The signed checksum set (ADR-0115) is the one publication gate: its
        # signature is verified against the pinned key before it is read, so
        # the expected binary hash is authenticated, not whatever the network
        # served, and a published set with no entry for the binary refuses.
        $sumsPath = Join-Path $staging 'SHA256SUMS'
        $sigPath = Join-Path $staging 'SHA256SUMS.sig'
        Get-ChocolateyWebFile -PackageName 'relate-sums' -FileFullPath $sumsPath -Url "$releaseUrl/SHA256SUMS"
        Get-ChocolateyWebFile -PackageName 'relate-sums-sig' -FileFullPath $sigPath -Url "$releaseUrl/SHA256SUMS.sig"
        Assert-RelateManifestSignature -Keygen $keygen -WorkDir $staging -SigPath $sigPath -SumsPath $sumsPath -Tag $tag
        $sums = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($sumsPath))
        Assert-RelateManifestVersion -Sums $sums -Tag $tag
        $expected = Get-RelateSumsEntry -Sums $sums -Asset $RelateAsset -Tag $tag

        # Download inside the staging directory, not into the package
        # directory: the binary only lands there once it has matched the
        # authenticated set and its signature.
        $downloadPath = Join-Path $staging $RelateAsset
        Get-ChocolateyWebFile -PackageName 'relate' -FileFullPath $downloadPath -Url "$releaseUrl/$RelateAsset"
        $actual = Get-RelateFileSha256 -Path $downloadPath
        if ($actual -ine $expected) {
            Stop-RelateChocolateyInstall ("checksum verification failed for $RelateAsset " +
                "(signed SHA256SUMS $expected, downloaded $actual); installing nothing")
        }
        Assert-RelateAuthenticodeSignature -Path $downloadPath

        Move-Item -LiteralPath $downloadPath -Destination $exePath -Force
    } finally {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }

    [IO.File]::WriteAllText(
        (Join-Path $RelateToolsDir $RelatePackageMarker),
        '{"manager":"chocolatey","upgrade":"choco upgrade relate"}'
    )

    # Expose the real binary on PATH through Chocolatey's shim directory.
    Install-BinFile -Name 'relate' -Path $exePath
}

Install-RelateChocolatey
