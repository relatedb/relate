# relate installer for Windows (ADR-0586):
#   irm https://relatedb.ai/install.ps1 | iex
# From cmd or Git Bash:
#   powershell -ExecutionPolicy ByPass -c "irm https://relatedb.ai/install.ps1 | iex"
#
# The Windows twin of install.sh: the same release resolution (releases.json,
# ADR-0571), the same direct download route with no GitHub API request and no
# token (ADR-0585), and the same fail-closed integrity chain - SSHSIG over
# SHA256SUMS against the pinned key (ADR-0115), the signed version header
# bound to the requested tag (ADR-0457), then the binary's SHA-256 - before
# anything is installed. Runs in Windows PowerShell 5.1 and PowerShell 7.
#
# The script only defines functions and constants; its last line is the one
# entry call. `iex` runs it in the caller's session, so a failure throws
# (never `exit`, which would close the caller's window) and every preference
# change stays inside Install-Relate's own scope.

# Pinned release signing public key (ADR-0115), byte-identical to install.sh's
# and distribution/release-signing-key.pub (the install harness's drift guard
# holds the copies together). Verification never fetches the key from the
# release.
$RelateSigningKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHgv/GHY5mtv/HopH/J/T7KrTT7eOVFEG2ueT4iRKwbK relate-release'

# The ADR-0457 signed-manifest version header, byte-identical to install.sh's
# and manifest.rs's SUMS_VERSION_PREFIX (one space after the colon is
# load-bearing).
$RelateSumsVersionPrefix = '# relate-release-version: '

# The last release published before the signed version header existed
# (ADR-0457): a headerless manifest is accepted only at or below it.
$RelateLegacyManifestCeiling = '0.14.0'

# The release-metadata URL (ADR-0250 sec. 1, ADR-0571). RELATE_RELEASES_JSON_URL
# overrides it (the dev/test seam install.sh and `relate upgrade` share);
# unset or empty means this canonical URL.
$RelateReleasesJsonUrl = 'https://relatedb.ai/releases.json'

# The direct release-asset download route (ADR-0585). RELATE_RELEASE_DOWNLOAD_URL
# overrides it; unset or empty means this canonical base. The override must be
# https:// - the same narrowing install.sh records.
$RelateDownloadBase = 'https://github.com/relatedb/relate/releases/download'

# The signer identity in allowed_signers and the SSH signature namespace:
# one value, `relate-release`, at both sites (ADR-0115).
$RelateSigner = 'relate-release'

$RelateBinName = 'relate.exe'

# The script's own path when it runs from a file, and empty under
# `irm | iex`, which runs it before anyone has read it.
$RelateInstallerPath = $PSCommandPath

# Every refusal: the message, prefixed for the console, with the bare
# message kept on the exception under $RelateMessageKey, so a caller that adds
# context composes from the typed value instead of re-parsing the text.
$RelateMessageKey = 'RelateMessage'
function Stop-RelateInstall {
    param([string] $Message)
    $refusal = New-Object System.Exception "install.ps1: $Message"
    $refusal.Data[$RelateMessageKey] = $Message
    throw $refusal
}

# The cause a caught error carries: a refusal's bare message, or, for any
# other exception, its own message. The one reader of $RelateMessageKey.
function Get-RelateRefusalCause {
    param([System.Management.Automation.ErrorRecord] $ErrorRecord)
    $cause = $ErrorRecord.Exception.Data[$RelateMessageKey]
    if ($cause) { return $cause }
    return $ErrorRecord.Exception.Message
}

# An untrusted value as a refusal may show it: every control character
# masked to '?', so no escape sequence reaches the console. The one display
# rule every refusal that quotes a value uses.
function Format-RelateShown {
    param([string] $Value)
    return ($Value -replace '[\x00-\x1f\x7f]', '?')
}

# The host's release-asset architecture. The binary is a Windows binary: any
# other OS gets install.sh's one-liner. A 32-bit PowerShell on 64-bit Windows
# reports x86 in PROCESSOR_ARCHITECTURE and the native architecture in
# PROCESSOR_ARCHITEW6432, so the latter wins. Windows 11 on ARM64 runs the
# amd64 build under its x64 emulation; Windows 10 on ARM64 cannot.
function Get-RelateHostArch {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Stop-RelateInstall 'this installer is for Windows; on macOS and Linux run: curl -sSfL https://relatedb.ai/install.sh | bash'
    }
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { return 'amd64' }
        'ARM64' {
            if ([Environment]::OSVersion.Version.Build -ge 22000) {
                Write-Host 'note: no native Windows ARM64 build yet; installing the amd64 build, which runs under x64 emulation'
                return 'amd64'
            }
            Stop-RelateInstall 'unsupported architecture: ARM64 before Windows 11 (the amd64 build needs Windows 11 x64 emulation)'
        }
    }
    Stop-RelateInstall "unsupported architecture: $arch. Supported: amd64 (and ARM64 on Windows 11 via x64 emulation)."
}

# Per-user install directory, the Windows convention for user-scope programs:
# %LOCALAPPDATA%\Programs\relate. No elevation, nothing written outside the
# user's profile.
function Get-RelateInstallDir {
    $base = $env:LOCALAPPDATA
    if (-not $base) {
        Stop-RelateInstall 'LOCALAPPDATA is not set; cannot resolve the install directory'
    }
    # Composed as text, not with Join-Path, so the one shape check below sees
    # the value first (5.1's path APIs throw on a quote or control character).
    $dir = $base.TrimEnd('\') + '\Programs\relate'
    # A precondition of the whole install, checked before the first request:
    # the directory must be rooted (never relative to where the pipe ran) and
    # writable as one PATH entry. One check; its refusal names LOCALAPPDATA.
    Assert-RelatePathEntry -Directory $dir -Origin 'LOCALAPPDATA' -OriginValue $base
    return $dir
}

# The one download transport. Every asset - the metadata document, the
# manifest, its signature, the binary - goes through here. No Authorization
# header is ever sent. Both runtimes refuse an https -> http redirect
# (.NET's redirect handling in 5.1 and 7 alike), so every hop stays on TLS. A
# non-2xx answer dies with the request and the code, the pinned status text
# install.sh and the CLI's release channel carry.
function Invoke-RelateDownload {
    param([string] $Url, [string] $Destination)
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing -ErrorAction Stop
    } catch {
        $response = $_.Exception.Response
        if ($null -ne $response) {
            Stop-RelateInstall (Get-RelateStatusMessage -Url $Url -Status ([int]$response.StatusCode))
        }
        Stop-RelateInstall "failed to fetch ${Url}; check the connection and retry ($($_.Exception.Message))"
    }
}

# The releases.json document at $MetadataUrl, as text. The release tag comes
# from it, never the GitHub API's latest-release endpoint, which can name an
# extension-channel release (ADR-0571); the document selects, it is never
# trusted. A failed fetch is one release-list sentence, composed from the
# transport's own refusal: Invoke-RelateDownload fails only through
# Stop-RelateInstall, which keeps the bare cause on the exception.
function Get-RelateReleaseDocument {
    param([string] $MetadataUrl, [string] $Destination)
    try {
        Invoke-RelateDownload -Url $MetadataUrl -Destination $Destination
    } catch {
        Stop-RelateInstall "$(Get-RelateReleaseListFailure -MetadataUrl $MetadataUrl): the fetch failed: $(Get-RelateRefusalCause $_)"
    }
    return [IO.File]::ReadAllText($Destination, [Text.Encoding]::UTF8)
}

# The value of the override seam $Name, or $Default when it is unset or
# empty, and https:// either way: the one gate both seams share. The
# metadata document only selects a release and is never trusted, but a
# plaintext fetch would let an on-path attacker steer which signed release
# installs, so it is TLS-only like the assets.
function Get-RelateHttpsSeam {
    param([string] $Name, [string] $Default)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if (-not $value) { $value = $Default }
    if (-not $value.StartsWith('https://', [StringComparison]::Ordinal)) {
        Stop-RelateInstall "$Name must be an https:// URL (got: $value)"
    }
    return $value
}

# The pinned non-2xx text, shared with install.sh and the CLI's release
# channel: one builder, so every site (and the harness's stubbed transport)
# speaks the same words.
function Get-RelateStatusMessage {
    param([string] $Url, [int] $Status)
    return "GET ${Url}: http status: $Status"
}

# The one stem every release-list failure starts with (install.sh's
# metadata_fail): the fetch failure and each parse failure name it.
function Get-RelateReleaseListFailure {
    param([string] $MetadataUrl)
    return "could not read the release list from $MetadataUrl"
}

# The latest stable release tag from a releases.json document (ADR-0250 sec. 2):
# the top-level latest.tag, validated as vX.Y.Z. A null latest, a missing or
# empty tag, and a tag of any other shape each fail with install.sh's
# message; nothing falls back to a guessed tag.
function Get-RelateLatestTag {
    param([string] $Document, [string] $MetadataUrl)
    $fail = Get-RelateReleaseListFailure -MetadataUrl $MetadataUrl
    $malformed = "${fail}: the document is malformed: no latest.tag"
    try {
        $doc = $Document | ConvertFrom-Json
    } catch {
        Stop-RelateInstall $malformed
    }
    if ($null -eq $doc -or -not ($doc.PSObject.Properties.Name -contains 'latest')) {
        Stop-RelateInstall $malformed
    }
    $latest = $doc.latest
    if ($null -eq $latest) {
        Stop-RelateInstall "${fail}: latest is null: no stable release is published"
    }
    # A missing tag reads as $null, which the string check refuses.
    $tag = $latest.tag
    if (-not ($tag -is [string]) -or $tag -eq '') {
        Stop-RelateInstall $malformed
    }
    # \A and \z, not ^ and $: .NET's $ also matches before a final newline,
    # which would admit "v1.2.3`n".
    if ($tag -cnotmatch '\Av[0-9]+\.[0-9]+\.[0-9]+\z') {
        Stop-RelateInstall "${fail}: latest.tag '$(Format-RelateShown $tag)' is not a vX.Y.Z release tag"
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
            Stop-RelateInstall "SHA256SUMS from $Tag carries an empty version header; installing nothing"
        }
        if ($sumsVersion -cne $Tag) {
            Stop-RelateInstall "SHA256SUMS is for $sumsVersion, not the requested release $Tag; installing nothing"
        }
    } elseif (Test-RelateVersionGreater ($Tag -replace '^v', '') $RelateLegacyManifestCeiling) {
        Stop-RelateInstall ("SHA256SUMS from $Tag has no signed version header; refusing to install a manifest " +
            "whose version is not authenticated (legacy manifests are accepted only at or below " +
            "v$RelateLegacyManifestCeiling); installing nothing")
    } else {
        Write-Warning ("install.ps1: SHA256SUMS from $Tag has no signed version header (legacy manifest, " +
            "accepted only at or below v$RelateLegacyManifestCeiling)")
    }
}

# The expected SHA-256 for $Asset from an authenticated SHA256SUMS. Lines are
# "<64-hex>  <name>" (an optional binary-mode asterisk tolerated); the first
# entry naming the asset is the one checked, and a missing entry fails.
function Get-RelateSumsEntry {
    param([string] $Sums, [string] $Asset, [string] $Tag)
    $entryPattern = '^([0-9a-fA-F]{64})[ \t]+\*?' + [regex]::Escape($Asset) + '$'
    foreach ($line in ($Sums -split "`n")) {
        $match = [regex]::Match($line.TrimEnd("`r"), $entryPattern)
        if ($match.Success) { return $match.Groups[1].Value }
    }
    Stop-RelateInstall "SHA256SUMS from $Tag has no entry for $Asset"
}

# $Child under the Windows System32 directory, or $null when SystemRoot is
# not set: the one derivation of the inbox location both callers use.
function Get-RelateSystem32Path {
    param([string] $Child)
    if (-not $env:SystemRoot) { return $null }
    return (Join-Path (Join-Path $env:SystemRoot 'System32') $Child)
}

# ssh-keygen: the inbox Windows OpenSSH client first (present by default
# since Windows 10 1809), PATH only when it is absent. The verifier decides
# whether a manifest is trusted, so the system copy wins over whatever a
# PATH entry earlier in the search order would resolve.
function Find-RelateSshKeygen {
    $inbox = Get-RelateSystem32Path 'OpenSSH\ssh-keygen.exe'
    if ($inbox -and (Test-Path -LiteralPath $inbox)) { return $inbox }
    $command = Get-Command ssh-keygen -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($command) { return $command.Path }
    return $null
}

# Verify the SSHSIG $SigPath over the manifest $SumsPath against the pinned
# key (ADR-0115), or fail with the authored refusal for $Tag. The whole
# trust step lives here: the allowed_signers file is written into $WorkDir
# from $RelateSigner and $RelateSigningKey, never fetched.
# `ssh-keygen -Y verify` reads the signed message only from stdin, and not
# every Windows build receives it intact from a .NET stdin pipe (Git for
# Windows' MSYS ssh-keygen did not). So cmd.exe's `<` hands it the manifest
# as a file handle, the documented form, and the exact bytes are verified (a
# PowerShell pipeline would re-encode them and rewrite line endings).
# Arguments go to cmd verbatim through ProcessStartInfo: PowerShell's own
# native-argument passing differs between 5.1 and 7. cmd would expand % and
# end a quoted path at ", so a path holding either fails closed.
function Assert-RelateManifestSignature {
    param([string] $Keygen, [string] $WorkDir, [string] $SigPath, [string] $SumsPath, [string] $Tag)
    $allowedSigners = Join-Path $WorkDir 'allowed_signers'
    foreach ($path in @($Keygen, $allowedSigners, $SigPath, $SumsPath)) {
        if ($path -match '["%]') {
            Stop-RelateInstall "cannot verify through a path holding a quote or a percent sign: $path"
        }
    }
    [IO.File]::WriteAllText($allowedSigners, "$RelateSigner $RelateSigningKey`n", (New-Object System.Text.UTF8Encoding $false))
    $shell = $env:ComSpec
    if (-not $shell) { $shell = Get-RelateSystem32Path 'cmd.exe' }
    if (-not $shell) {
        Stop-RelateInstall 'cannot locate cmd.exe to verify the signature: neither ComSpec nor SystemRoot is set'
    }
    $template = '"{0}" -Y verify -f "{1}" -I {2} -n {2} -s "{3}" < "{4}"'
    $verify = $template -f $Keygen, $allowedSigners, $RelateSigner, $SigPath, $SumsPath
    # /s strips the outer quote pair and runs the rest as written.
    if ((Invoke-RelateProcess -FilePath $shell -Arguments ('/d /s /c "' + $verify + '"')) -ne 0) {
        Stop-RelateInstall "SHA256SUMS signature verification failed for $Tag; installing nothing"
    }
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

# Stage the verified binary inside the install directory (same volume) and
# move it into place, so the final path never holds a partial binary. A
# running relate.exe cannot be overwritten or deleted on Windows but can be
# renamed, so an existing binary is first retired to a unique .old- name; if
# the new binary cannot take its place, the old one is moved back, and if even
# that fails the error names where the old binary was kept. Retired binaries
# still running are removed by a later install.
function Install-RelateBinary {
    param([string] $Source, [string] $InstallDir)
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $target = Join-Path $InstallDir $RelateBinName
    $staged = Join-Path $InstallDir (".$RelateBinName." + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        [IO.File]::Copy($Source, $staged)
        $retired = $null
        if (Test-Path -LiteralPath $target) {
            $retired = "$target.old-" + [Guid]::NewGuid().ToString('N')
            [IO.File]::Move($target, $retired)
        }
        try {
            [IO.File]::Move($staged, $target)
        } catch {
            $failure = $_
            if ($retired) {
                try {
                    [IO.File]::Move($retired, $target)
                } catch {
                    Stop-RelateInstall ("could not move the new $RelateBinName into place ($($failure.Exception.Message)) " +
                        "nor restore the previous one ($($_.Exception.Message)); the previous binary is at $retired")
                }
            }
            throw $failure
        }
    } finally {
        if (Test-Path -LiteralPath $staged) {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        }
    }
    Get-ChildItem -LiteralPath $InstallDir -Filter "$RelateBinName.old-*" -Force -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    return $target
}

# True when one of the PATH entries $Entries names $Directory: compared
# case-insensitively, after expanding %VAR% references and ignoring a
# trailing backslash. The one membership rule for the persistent user PATH
# and this session's PATH.
function Test-RelatePathEntry {
    param([string[]] $Entries, [string] $Directory)
    $wanted = $Directory.TrimEnd('\')
    foreach ($entry in $Entries) {
        if ($entry -and [Environment]::ExpandEnvironmentVariables($entry).TrimEnd('\') -ieq $wanted) {
            return $true
        }
    }
    return $false
}

# Fail unless $Directory is safe to write as one PATH entry: free of ';' (it
# would split into several entries), '"', '%' (the value is written as
# REG_EXPAND_SZ, so it would expand) and control characters, and rooted (a
# relative entry would resolve against whatever directory a shell starts
# in). The characters are checked first: .NET Framework's IsPathRooted
# throws on a quote or a control character instead of answering. The one
# refusal is worded here: for the variable the directory came from when the
# caller names it ($Origin, with its raw $OriginValue), else for the
# directory itself.
function Assert-RelatePathEntry {
    param([string] $Directory, [string] $Origin = '', [string] $OriginValue = '')
    $reason = $null
    if ($Directory -match '[;"%\x00-\x1f]') {
        $reason = "it holds ';', a quote, '%' or a control character"
    } elseif (-not [IO.Path]::IsPathRooted($Directory)) {
        $reason = 'it is relative'
    }
    if (-not $reason) { return }
    if ($Origin) {
        Stop-RelateInstall "$Origin must be an absolute directory usable on PATH (got: $(Format-RelateShown $OriginValue)): $reason"
    }
    Stop-RelateInstall "not adding $(Format-RelateShown $Directory) to PATH: $reason"
}

# Add $Directory to the user's persistent PATH (HKCU\Environment) unless an
# entry already names it; $true when PATH changed. The value is read without
# expanding %VAR% references and written back as REG_EXPAND_SZ, so the
# user's existing entries keep their form. Deleting a never-set variable
# through [Environment] broadcasts WM_SETTINGCHANGE, so shells started after
# the install see the new PATH without a sign-out; the variable's name is
# unique per install, so it can never be one of the user's own.
function Add-RelateUserPath {
    param([string] $Directory)
    # Opens the key writable, creating it when absent.
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        # Only a string-typed Path is rewritten: any other registry kind is
        # left as it is, and the caller reports the PATH step as not done.
        if ($key.GetValueNames() -contains 'Path') {
            $kind = $key.GetValueKind('Path')
            if ($kind -ne [Microsoft.Win32.RegistryValueKind]::String -and
                $kind -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) {
                throw "the user Path value is a $kind, not a string; it was left unchanged"
            }
        }
        $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $entries = @($raw -split ';' | Where-Object { $_ -ne '' })
        if (Test-RelatePathEntry -Entries $entries -Directory $Directory) {
            return $false
        }
        $key.SetValue('Path', (@($entries + $Directory) -join ';'), [Microsoft.Win32.RegistryValueKind]::ExpandString)
    } finally {
        $key.Close()
    }
    # Best effort: PATH is already written, so a failed broadcast only means
    # new shells pick it up after the next sign-in; it is never reported as a
    # failed PATH update.
    $broadcast = 'RELATE_INSTALL_PATH_BROADCAST_' + [Guid]::NewGuid().ToString('N')
    try {
        [Environment]::SetEnvironmentVariable($broadcast, $null, 'User')
    } catch {
        Write-Verbose "install.ps1: could not broadcast the PATH change: $($_.Exception.Message)"
    }
    return $true
}

# The fail-closed integrity chain (ADR-0115, ADR-0457), in its one fixed
# order: the small manifest and its signature download first; the signature
# is verified against the pinned key; the signed version header is bound to
# $Tag; the manifest's entry for $Asset is looked up; only then does the
# binary download, and its SHA-256 must match. Returns the verified binary's
# path under $WorkDir, or throws having installed nothing - a missing,
# invalid or wrong-key signature never downloads the binary.
function Get-RelateVerifiedBinary {
    param([string] $DownloadBase, [string] $Tag, [string] $Asset, [string] $Keygen, [string] $WorkDir)
    $sumsPath = Join-Path $WorkDir 'SHA256SUMS'
    $sigPath = Join-Path $WorkDir 'SHA256SUMS.sig'
    # The release's direct-route base, composed once.
    $releaseUrl = "$DownloadBase/$Tag"
    Invoke-RelateDownload -Url "$releaseUrl/SHA256SUMS" -Destination $sumsPath
    Invoke-RelateDownload -Url "$releaseUrl/SHA256SUMS.sig" -Destination $sigPath

    Assert-RelateManifestSignature -Keygen $Keygen -WorkDir $WorkDir -SigPath $sigPath -SumsPath $sumsPath -Tag $Tag
    $sums = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($sumsPath))
    Assert-RelateManifestVersion -Sums $sums -Tag $Tag
    $expected = Get-RelateSumsEntry -Sums $sums -Asset $Asset -Tag $Tag

    $binaryPath = Join-Path $WorkDir $Asset
    Invoke-RelateDownload -Url "$releaseUrl/$Asset" -Destination $binaryPath
    $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if ($actual -ine $expected) {
        Stop-RelateInstall "checksum verification failed for $Asset; installing nothing"
    }
    return $binaryPath
}

function Install-Relate {
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell's progress bar makes Invoke-WebRequest many times
    # slower; this scope only.
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 on an older .NET defaults to SSL3/TLS 1.0, which
    # GitHub refuses. Only a pinned protocol list is widened; the OS default
    # (0, SystemDefault) already negotiates TLS 1.2. PowerShell 7 ignores it.
    if ($PSVersionTable.PSEdition -ne 'Core') {
        $protocols = [Net.ServicePointManager]::SecurityProtocol
        if ([int]$protocols -ne 0 -and -not ($protocols -band [Net.SecurityProtocolType]::Tls12)) {
            [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
        }
    }

    if (-not $RelateInstallerPath) {
        Write-Host ('install.ps1: piped into PowerShell, so this installer ran before you could read it; ' +
            'to review it first, see https://github.com/relatedb/relate#install')
    }

    $arch = Get-RelateHostArch
    $asset = "relate-windows-$arch.exe"
    $installDir = Get-RelateInstallDir

    # Both seams are gated before the first request of any kind, so a
    # mis-set override dies before any network hop (ADR-0585, ADR-0586).
    $metadataUrl = Get-RelateHttpsSeam -Name 'RELATE_RELEASES_JSON_URL' -Default $RelateReleasesJsonUrl
    $downloadBase = Get-RelateHttpsSeam -Name 'RELATE_RELEASE_DOWNLOAD_URL' -Default $RelateDownloadBase

    $keygen = Find-RelateSshKeygen
    if (-not $keygen) {
        Stop-RelateInstall ('ssh-keygen (the Windows OpenSSH client) is required to verify release signatures. ' +
            'Install it from an elevated PowerShell with: Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0 ' +
            '- then retry.')
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('relate-install-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $document = Get-RelateReleaseDocument -MetadataUrl $metadataUrl -Destination (Join-Path $tmp 'releases.json')
        $tag = Get-RelateLatestTag -Document $document -MetadataUrl $metadataUrl

        Write-Host "Installing relate $tag (windows/$arch) to $installDir"

        $binaryPath = Get-RelateVerifiedBinary -DownloadBase $downloadBase -Tag $tag -Asset $asset `
            -Keygen $keygen -WorkDir $tmp
        $target = Install-RelateBinary -Source $binaryPath -InstallDir $installDir
        Write-Host "Installed relate $tag at $target"
        # The binary is verified and in place; a PATH failure past this point
        # is reported as such, never as a failed install.
        # The directory's PATH shape was asserted with it, before any request.
        try {
            if (Add-RelateUserPath -Directory $installDir) {
                Write-Host "Added $installDir to your user PATH; open a new terminal to use relate."
            }
            # This session too, when it does not already resolve the directory.
            if (-not (Test-RelatePathEntry -Entries ($env:Path -split ';') -Directory $installDir)) {
                $env:Path = "$installDir;$env:Path"
            }
        } catch {
            Write-Warning ("install.ps1: relate is installed, but the user PATH could not be updated " +
                "($($_.Exception.Message)); add $installDir to PATH yourself")
        }
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Relate
