# Offline, hermetic test harness for distribution/install.ps1 (ADR-0586).
#
# Runs in Windows PowerShell 5.1 and PowerShell 7 (the CI Windows lane runs
# it under both). The installer's definitions are loaded without its final
# entry call, then three seams are replaced: the download transport serves
# fixtures keyed on the request URL (any other URL answers 404, so a GitHub
# API request fails the install loudly), the architecture probe reports
# amd64, and the user-PATH writer records its argument instead of touching
# the registry. Everything else is the real installer - in particular the
# real ssh-keygen verifies real SSHSIG signatures over the fixture manifests,
# fed through the installer's own cmd.exe redirection, against a throwaway
# key that replaces the pinned one. LOCALAPPDATA points into the sandbox,
# which is removed on exit. Windows only: the verification path is cmd.exe's.
# The stubs, the fixture factories and the per-case runner live in
# scripts/tests/lib/install_ps1_harness.ps1, the TAP runtime in
# scripts/tests/lib/tap.ps1; this file holds the fixtures and the cases.
#
# The constants the installer shares with install.sh and the CLI are held
# together by scripts/tests/test_install_sh.sh's drift guards, which run on
# every host without PowerShell.
#
# Output and exit contract: TAP, as scripts/tests/lib/tap.sh - numbered
# ok/not ok lines, a plan and summary footer, exit 1 when anything failed.
#
# Usage: pwsh -NoProfile -File scripts/tests/test_install_ps1.ps1
#        powershell -NoProfile -ExecutionPolicy Bypass -File scripts/tests/test_install_ps1.ps1

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Installer = Join-Path (Join-Path $Root 'distribution') 'install.ps1'

# --- 0. Load the installer's definitions, then the harness ------------------

# One line scan: the last non-blank line must be exactly the entry call, with
# a non-empty definitions prefix before it, which is all this harness loads.
$installerLines = @([IO.File]::ReadAllLines($Installer))
$entry = $installerLines.Count - 1
while ($entry -ge 0 -and $installerLines[$entry].Trim() -eq '') { $entry-- }
if (-not ($entry -gt 0 -and $installerLines[$entry] -ceq 'Install-Relate')) {
    Write-Output 'Bail out! install.ps1 must end with its one entry call after its definitions, so they can be loaded alone'
    exit 1
}
. ([ScriptBlock]::Create(($installerLines[0..($entry - 1)] -join "`n")))

# The TAP runtime, then the harness machinery, which replaces three
# installer functions and so loads after the installer's definitions.
. (Join-Path (Join-Path $PSScriptRoot 'lib') 'tap.ps1')
. (Join-Path (Join-Path $PSScriptRoot 'lib') 'install_ps1_harness.ps1')
Test-Check 'install.ps1 ends with its one entry call after its definitions' $true

# Windows only: the installer verifies signatures through cmd.exe.
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    Write-Output 'Bail out! the install.ps1 harness runs on Windows only (signature checks go through cmd.exe)'
    exit 1
}
$Sandbox = Join-Path ([IO.Path]::GetTempPath()) ('relate-install-ps1-test-' + [Guid]::NewGuid().ToString('N'))
$Fixtures = Join-Path $Sandbox 'fixtures'
New-Item -ItemType Directory -Path $Fixtures | Out-Null

try {
    # --- 1. Signature-check path guard ------------------------------------------

    foreach ($bad in @('C:\Temp\100%\SHA256SUMS', 'C:\Temp\a"b\SHA256SUMS')) {
        Test-Throws "signature check: a path cmd would rewrite fails closed ($bad)" `
            { Assert-RelateManifestSignature -Keygen 'ssh-keygen' -WorkDir 'C:\x' -SigPath 'C:\y' -SumsPath $bad -Tag 'v1.2.3' } `
            'cannot verify through a path holding a quote or a percent sign'
    }

    # --- 2. Version comparison -----------------------------------------------

    Test-Check 'version: 0.14.1 > 0.14.0' (Test-RelateVersionGreater '0.14.1' '0.14.0')
    Test-Check 'version: 0.14.0 is not > 0.14.0' (-not (Test-RelateVersionGreater '0.14.0' '0.14.0'))
    Test-Check 'version: 0.9.0 is not > 0.14.0' (-not (Test-RelateVersionGreater '0.9.0' '0.14.0'))
    Test-Check 'version: 10.0.0 > 9.99.99' (Test-RelateVersionGreater '10.0.0' '9.99.99')
    Test-Check 'version: unparseable counts as greater (fail closed)' (Test-RelateVersionGreater '1.x.0' '0.14.0')

    # --- 3. PATH membership -----------------------------------------------------

    Test-Check 'PATH: a trailing backslash and case still match' `
        (Test-RelatePathEntry -Entries @('C:\Other', 'c:\users\u\programs\RELATE\') -Directory 'C:\Users\u\Programs\relate')
    Test-Check 'PATH: an absent directory does not match' `
        (-not (Test-RelatePathEntry -Entries @('C:\Other', '') -Directory 'C:\Users\u\Programs\relate'))
    # LOCALAPPDATA, the install directory's base, is validated on its own.
    $savedLocalAppData = $env:LOCALAPPDATA
    try {
        foreach ($badBase in @('Programs', 'C:\a"b')) {
            $env:LOCALAPPDATA = $badBase
            Test-Throws "install dir: an unusable LOCALAPPDATA is refused ($badBase)" `
                { Get-RelateInstallDir } 'LOCALAPPDATA must be an absolute directory'
        }
    } finally {
        $env:LOCALAPPDATA = $savedLocalAppData
    }

    Test-NoThrow 'PATH entry: a rooted plain directory is accepted' `
        { Assert-RelatePathEntry -Directory 'C:\Users\u\AppData\Local\Programs\relate' }
    Test-Throws 'PATH entry: a relative directory is refused' `
        { Assert-RelatePathEntry -Directory 'Programs\relate' } 'to PATH: it is relative'
    foreach ($unsafe in @('C:\a;C:\b', 'C:\a"b', 'C:\%TEMP%\relate', "C:\a`tb")) {
        Test-Throws "PATH entry: a directory that would corrupt PATH is refused ($($unsafe -replace '\t', '\t'))" `
            { Assert-RelatePathEntry -Directory $unsafe } "to PATH"
    }

    # --- 4. Manifest binding and entry lookup, directly ------------------------

    $hash = 'a' * 64
    $body = "$hash  relate-windows-amd64.exe`n$('b' * 64)  relate-linux-amd64`n"
    Test-NoThrow 'binding: a matching header passes' `
        { Assert-RelateManifestVersion -Sums "${RelateSumsVersionPrefix}v1.2.3`n$body" -Tag 'v1.2.3' }
    Test-NoThrow 'binding: a CRLF header still binds' `
        { Assert-RelateManifestVersion -Sums "${RelateSumsVersionPrefix}v1.2.3`r`n$body" -Tag 'v1.2.3' }
    Test-Throws 'binding: another release in the header fails' `
        { Assert-RelateManifestVersion -Sums "${RelateSumsVersionPrefix}v1.2.2`n$body" -Tag 'v1.2.3' } 'SHA256SUMS is for v1.2.2'
    Test-Throws 'binding: a respelled prefix is headerless, refused above the ceiling' `
        { Assert-RelateManifestVersion -Sums "#relate-release-version: v1.2.3`n$body" -Tag 'v1.2.3' } 'has no signed version header; refusing'
    Test-NoThrow 'binding: headerless at the ceiling passes' `
        { Assert-RelateManifestVersion -Sums $body -Tag "v$RelateLegacyManifestCeiling" }
    Test-Check 'entry: the asset line is found' `
        ((Get-RelateSumsEntry -Sums $body -Asset 'relate-windows-amd64.exe' -Tag 'v1.2.3') -ceq $hash)
    Test-Check 'entry: a binary-mode asterisk and CRLF are tolerated' `
        ((Get-RelateSumsEntry -Sums "$hash *relate-windows-amd64.exe`r`n" -Asset 'relate-windows-amd64.exe' -Tag 'v1.2.3') -ceq $hash)
    Test-Throws 'entry: a longer name is not a match' `
        { Get-RelateSumsEntry -Sums "$hash  relate-windows-amd64.exe.bak`n" -Asset 'relate-windows-amd64.exe' -Tag 'v1.2.3' } `
        'has no entry for relate-windows-amd64.exe'

    # --- 5. Fixtures: keys, signed manifests, release documents ----------------

    $keygen = Find-RelateSshKeygen
    Test-Check 'ssh-keygen is available to the harness' ([bool]$keygen)
    Write-Output "# ssh-keygen: $keygen"
    if (-not $keygen) { throw 'ssh-keygen is required for the signature roundtrip' }

    $signingKey = New-TestKey 'signing-key'
    $wrongKey = New-TestKey 'wrong-key'

    # The harness key replaces the pinned one; everything else is pinned.
    $RelateSigningKey = ([IO.File]::ReadAllText("$signingKey.pub").Trim() -split ' ')[0..1] -join ' '

    $tag = 'v9.9.9'
    $asset = 'relate-windows-amd64.exe'
    $binary = Write-Fixture $asset 'relate fixture windows-amd64'
    $corrupt = Write-Fixture "$asset.corrupt" 'relate fixture windows-amd64 corrupt-bytes'
    $binaryHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    $entries = "$binaryHash  $asset`n$('0' * 64)  relate-linux-amd64`n"

    $good = New-SignedSums 'SHA256SUMS' "$RelateSumsVersionPrefix$tag`n$entries"
    $wrongVersion = New-SignedSums 'SHA256SUMS.wrong-version' "${RelateSumsVersionPrefix}v9.9.8`n$entries"
    $emptyHeader = New-SignedSums 'SHA256SUMS.empty-header' "$RelateSumsVersionPrefix`n$entries"
    $headerless = New-SignedSums 'SHA256SUMS.headerless' $entries
    $noEntry = New-SignedSums 'SHA256SUMS.no-entry' "$RelateSumsVersionPrefix$tag`n$('0' * 64)  relate-linux-amd64`n"
    $wrongSigner = New-SignedSums 'SHA256SUMS.wrong-signer' "$RelateSumsVersionPrefix$tag`n$entries" $wrongKey
    $legacyTag = "v$RelateLegacyManifestCeiling"
    $legacy = New-SignedSums 'SHA256SUMS.legacy' $entries
    # Tampered after signing: the good signature over different bytes.
    $tampered = Write-Fixture 'SHA256SUMS.tampered' ("$RelateSumsVersionPrefix$tag`n" + $entries.Replace($binaryHash, $binaryHash.Substring(1) + 'f'))
    $garbageSig = Write-Fixture 'SHA256SUMS.garbage-sig' "-----BEGIN SSH SIGNATURE-----`nnot a signature`n-----END SSH SIGNATURE-----`n"
    # The same bytes with CRLF line endings: what a text-mode pipe would feed.
    $crlf = Write-Fixture 'SHA256SUMS.crlf' (([IO.File]::ReadAllText($good.Sums)) -replace "`n", "`r`n")

    $releases = New-ReleasesDoc 'releases.json' "{ `"tag`": `"$tag`", `"prerelease`": false }"
    $releasesLegacy = New-ReleasesDoc 'releases.legacy.json' "{ `"tag`": `"$legacyTag`", `"prerelease`": false }"
    $releasesNull = New-ReleasesDoc 'releases.null.json' 'null'
    $releasesNoTag = New-ReleasesDoc 'releases.no-tag.json' '{ "prerelease": false }'
    $releasesBadTag = New-ReleasesDoc 'releases.bad-tag.json' '{ "tag": "latest" }'
    # A trailing newline after a well-formed tag: .NET's $ would admit it.
    $releasesNewlineTag = New-ReleasesDoc 'releases.newline-tag.json' "{ `"tag`": `"$tag\n`" }"
    $releasesMalformed = Write-Fixture 'releases.malformed.json' '{ "latest": '

    # --- 6. Happy path, reinstall, PATH failure --------------------------------

    $run = Invoke-InstallCase (Get-DefaultRelease)
    Test-Succeeded 'happy path: the installer succeeds' $run
    Test-Check 'happy path: the verified binary is installed as relate.exe' (Test-Installed $run $binary)
    Test-Check 'happy path: exactly the metadata document and three assets are requested' ($run.Calls.Count -eq 4)
    Test-Check 'happy path: the metadata comes from releases.json' ($run.Calls[0] -ceq $RelateReleasesJsonUrl)
    Test-Check 'happy path: no GitHub API request' `
        (@($run.Calls | Where-Object { $_ -match 'api\.github\.com|/releases/(tags|assets|latest)' }).Count -eq 0)
    Test-Check 'happy path: the install directory is %LOCALAPPDATA%\Programs\relate' `
        ($run.Dir -ceq (Join-Path (Join-Path $run.LocalAppData 'Programs') 'relate'))
    Test-Check 'happy path: that directory goes on the user PATH' `
        ($run.PathAdds.Count -eq 1 -and $run.PathAdds[0] -ceq $run.Dir)
    Test-Check 'happy path: the output names the tag and the installed path' `
        ($run.Output.Contains("Installed relate $tag at $($run.Exe)"))
    Test-Check 'happy path: no staging or retired files are left behind' `
        (Test-OnlyBinaryIn $run.Dir)

    # Piped execution (no script path, as under `irm | iex`) says so once and
    # still installs; a run from a file stays quiet.
    $RelateInstallerPath = ''
    $run = Invoke-InstallCase (Get-DefaultRelease)
    Test-Succeeded 'piped run: the installer succeeds' $run `
        ($run.Output.Contains('so this installer ran before you could read it'))
    $RelateInstallerPath = 'C:\relate\install.ps1'
    $run = Invoke-InstallCase (Get-DefaultRelease)
    Test-Succeeded 'run from a file: no piped-execution note' $run `
        (-not $run.Output.Contains('so this installer ran before you could read it'))

    # Reinstall over an existing binary: replaced in place, nothing retired
    # left behind.
    $first = Invoke-InstallCase (Get-DefaultRelease)
    # The precondition is checked, never guarded: a broken first install
    # fails here, and the stale-binary write below then fails the run.
    Test-Succeeded 'reinstall: the first install put relate.exe in place' $first (Test-Path -LiteralPath $first.Exe)
    Write-TestFile -Path $first.Exe -Text 'an older relate'
    $again = Invoke-InstallCase (Get-DefaultRelease) @{ LOCALAPPDATA = $first.LocalAppData }
    Test-Succeeded 'reinstall: the installer succeeds' $again
    Test-Check 'reinstall: the existing relate.exe is replaced' (Test-Installed $again $binary)
    Test-Check 'reinstall: no retired or staged binary is left behind' `
        (Test-OnlyBinaryIn $again.Dir)

    # A PATH update failure after the binary is in place is a warning, never
    # a failed install.
    $run = Invoke-WithStub 'Add-RelateUserPath' { throw 'registry unavailable' } { Invoke-InstallCase (Get-DefaultRelease) }
    Test-Succeeded 'PATH failure: the install still succeeds' $run (Test-Installed $run $binary)
    Test-Check 'PATH failure: it is reported as a PATH warning' `
        (($run.Warnings -join "`n").Contains('relate is installed, but the user PATH could not be updated'))

    # --- 7. Signature verification (ADR-0115) ----------------------------------

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest @{ Sums = $tampered; Sig = $good.Sig })
    Test-Refused 'tampered SHA256SUMS' $run "SHA256SUMS signature verification failed for $tag; installing nothing"

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest @{ Sums = $good.Sums; Sig = $garbageSig })
    Test-Refused 'tampered signature' $run 'SHA256SUMS signature verification failed'

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest $wrongSigner)
    Test-Refused 'signature by another key' $run 'SHA256SUMS signature verification failed'

    # The manifest must reach ssh-keygen byte-exact: the same manifest with
    # CRLF line endings is different bytes and must not verify.
    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest @{ Sums = $crlf; Sig = $good.Sig })
    Test-Refused 'CRLF-rewritten SHA256SUMS' $run 'SHA256SUMS signature verification failed'

    $serve = Get-DefaultRelease
    $serve.Remove("$RelateDownloadBase/$tag/SHA256SUMS.sig")
    $run = Invoke-InstallCase $serve
    Test-Refused 'missing signature asset' $run "GET $RelateDownloadBase/$tag/SHA256SUMS.sig: http status: 404"

    $run = Invoke-WithStub 'Find-RelateSshKeygen' { return $null } { Invoke-InstallCase (Get-DefaultRelease) }
    Test-Refused 'no ssh-keygen' $run 'Add-WindowsCapability -Online -Name OpenSSH.Client'
    Test-Check 'no ssh-keygen: refused before any request' ($run.Calls.Count -eq 0)
    Test-Check 'no ssh-keygen: the stub ended with its case' ((Find-RelateSshKeygen) -ceq $keygen)

    # --- 8. Signed version binding (ADR-0457) ----------------------------------

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest $wrongVersion)
    Test-Refused 'version header mismatch' $run "SHA256SUMS is for v9.9.8, not the requested release $tag; installing nothing"

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest $emptyHeader)
    Test-Refused 'empty version header' $run 'carries an empty version header'

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest $headerless)
    Test-Refused 'headerless manifest above the legacy ceiling' $run 'has no signed version header; refusing'

    $run = Invoke-InstallCase (Get-DefaultRelease -ReleaseTag $legacyTag -Manifest $legacy -ReleasesDoc $releasesLegacy)
    Test-Succeeded 'headerless manifest at the legacy ceiling: installs' $run (Test-Installed $run $binary)
    Test-Check 'headerless manifest at the legacy ceiling: warns' (($run.Warnings -join "`n").Contains('legacy manifest'))

    # --- 9. Checksum and manifest entry ----------------------------------------

    # Refused after the download: the mismatch is only knowable once the
    # binary arrived.
    $run = Invoke-InstallCase (Get-DefaultRelease -BinaryPath $corrupt)
    Test-Check 'tampered binary: the installer refuses' `
        ($null -ne $run.Error -and $run.Error.Contains("checksum verification failed for $asset; installing nothing"))
    Test-Check 'tampered binary: nothing is installed' (-not (Test-Path -LiteralPath $run.Exe))
    Test-Check 'tampered binary: it was downloaded exactly once' ((Get-BinaryCalls $run).Count -eq 1)

    $run = Invoke-InstallCase (Get-DefaultRelease -Manifest $noEntry)
    Test-Refused 'manifest without the asset' $run "SHA256SUMS from $tag has no entry for $asset"

    # --- 10. Release resolution (ADR-0571) -------------------------------------

    $run = Invoke-InstallCase (Get-DefaultRelease -ReleasesDoc $releasesNull)
    Test-Refused 'null latest' $run 'latest is null: no stable release is published'
    $run = Invoke-InstallCase (Get-DefaultRelease -ReleasesDoc $releasesNoTag)
    Test-Refused 'latest without a tag' $run 'the document is malformed: no latest.tag'
    $run = Invoke-InstallCase (Get-DefaultRelease -ReleasesDoc $releasesMalformed)
    Test-Refused 'malformed releases.json' $run 'the document is malformed: no latest.tag'
    $run = Invoke-InstallCase (Get-DefaultRelease -ReleasesDoc $releasesBadTag)
    Test-Refused 'non-vX.Y.Z latest tag' $run "latest.tag 'latest' is not a vX.Y.Z release tag"
    $run = Invoke-InstallCase (Get-DefaultRelease -ReleasesDoc $releasesNewlineTag)
    Test-Refused 'latest tag with a trailing newline' $run 'is not a vX.Y.Z release tag'
    $run = Invoke-InstallCase @{}
    Test-Refused 'metadata fetch failure' $run "could not read the release list from ${RelateReleasesJsonUrl}: the fetch failed: GET ${RelateReleasesJsonUrl}: http status: 404"

    $metaOverride = 'https://metadata.invalid/releases.json'
    $run = Invoke-InstallCase (Get-DefaultRelease -MetadataUrl $metaOverride) @{ RELATE_RELEASES_JSON_URL = $metaOverride }
    Test-Succeeded 'RELATE_RELEASES_JSON_URL: the override document is read' $run ($run.Calls[0] -ceq $metaOverride)

    # --- 11. Download-base override (RELATE_RELEASE_DOWNLOAD_URL) -------------

    $mirror = 'https://mirror.invalid/download'
    $run = Invoke-InstallCase (Get-DefaultRelease -Base $mirror) @{ RELATE_RELEASE_DOWNLOAD_URL = $mirror }
    Test-Succeeded 'download override: every asset comes from the override base' $run `
        (@($run.Calls | Select-Object -Skip 1 | Where-Object { -not $_.StartsWith("$mirror/$tag/") }).Count -eq 0)

    $run = Invoke-InstallCase (Get-DefaultRelease) @{ RELATE_RELEASE_DOWNLOAD_URL = 'http://mirror.invalid/download' }
    Test-Refused 'plain-http download override' $run 'RELATE_RELEASE_DOWNLOAD_URL must be an https:// URL'
    Test-Check 'plain-http download override: refused before any request' ($run.Calls.Count -eq 0)

    $run = Invoke-InstallCase (Get-DefaultRelease) @{ RELATE_RELEASES_JSON_URL = 'http://metadata.invalid/releases.json' }
    Test-Refused 'plain-http metadata override' $run 'RELATE_RELEASES_JSON_URL must be an https:// URL'
    Test-Check 'plain-http metadata override: refused before any request' ($run.Calls.Count -eq 0)

    # --- 12. Platform probe ---------------------------------------------------

    $probe = $null
    try { $probe = & $realHostArch } catch { $probe = $_.Exception.Message }
    Test-Check "platform probe: this Windows host maps to a release architecture (got: $probe)" ($probe -ceq 'amd64')
} finally {
    Remove-Item -LiteralPath $Sandbox -Recurse -Force -ErrorAction SilentlyContinue
}

Exit-TapRun
