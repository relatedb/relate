# Harness machinery for the install.ps1 test harness
# (scripts/tests/test_install_ps1.ps1): the fixture and signature
# factories, the three seam stubs, and the per-case run harness. Its
# assertions report through scripts/tests/lib/tap.ps1, which the case script
# loads first.
# The install.sh twin is scripts/tests/lib/install_sh_harness.sh.
#
# Dot-sourced by the case script after it has loaded the installer's
# definitions: the stubs below replace three installer functions, so they
# must be defined after them. Expects from the caller: $Fixtures (the
# fixture directory), $Sandbox (per-case LOCALAPPDATA roots), and, by the
# time the factories and the runner run, $keygen, $signingKey, $tag, $asset,
# $binary, $good and $releases (the default release the runner serves).
# Never executed directly.

# Write $Text to $Path as UTF-8 without a BOM: the one file writer every
# fixture and case uses.
function Write-TestFile {
    param([string] $Path, [string] $Text)
    [IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding $false))
}

# Write $Text as the fixture $Name under $Fixtures; returns its path.
function Write-Fixture {
    param([string] $Name, [string] $Text)
    $path = Join-Path $Fixtures $Name
    Write-TestFile -Path $path -Text $Text
    return $path
}

# Run a fixture command through the installer's one process launcher and
# fail the harness, naming $Label, when it exits non-zero: the one
# run-and-check policy for key generation, signing and the key ACL. Every
# argument is a constant or a path this harness created (no quote, no
# trailing backslash), so wrapping each in quotes is the whole quoting rule;
# it also keeps the empty `-N ''` passphrase, which Windows PowerShell 5.1's
# own native-argument passing would drop.
function Invoke-TestNative {
    param([string] $FilePath, [string[]] $ArgumentList, [string] $Label)
    $arguments = (@($ArgumentList | ForEach-Object { '"' + $_ + '"' }) -join ' ')
    $code = Invoke-RelateProcess -FilePath $FilePath -Arguments $arguments
    if ($code -ne 0) { throw "$Label failed (exit $code)" }
}

# Generate an Ed25519 keypair named $Name under $Fixtures; returns the
# private key path.
function New-TestKey {
    param([string] $Name)
    $path = Join-Path $Fixtures $Name
    Invoke-TestNative -FilePath $keygen -Label "ssh-keygen generating $Name" `
        -ArgumentList @('-q', '-t', 'ed25519', '-N', '', '-C', $RelateSigner, '-f', $path)
    # Windows OpenSSH refuses a private key other principals can read.
    Invoke-TestNative -FilePath 'icacls.exe' -Label "icacls restricting $Name to the current user" `
        -ArgumentList @($path, '/inheritance:r', '/grant:r', "${env:USERNAME}:F")
    return $path
}

# Sign $Path with $Key into "$Path.sig"; returns the signature path.
function New-Signature {
    param([string] $Path, [string] $Key)
    Invoke-TestNative -FilePath $keygen -Label "ssh-keygen signing $Path" `
        -ArgumentList @('-q', '-Y', 'sign', '-f', $Key, '-n', $RelateSigner, $Path)
    return "$Path.sig"
}

# Each manifest variant is signed on its own, so every failure below is
# the variant's own defect, never a stale signature.
function New-SignedSums {
    param([string] $Name, [string] $Text, [string] $Key = $signingKey)
    $path = Write-Fixture $Name $Text
    $sig = New-Signature $path $Key
    return @{ Sums = $path; Sig = $sig }
}

# Write a releases.json document whose latest slot is $LatestJson.
function New-ReleasesDoc {
    param([string] $Name, [string] $LatestJson)
    return Write-Fixture $Name ("{`n  `"schema_version`": 1,`n  `"latest`": $LatestJson,`n" +
        "  `"latest_prerelease`": { `"tag`": `"v0.11.0-rc.1`", `"prerelease`": true },`n  `"releases`": []`n}`n")
}

# --- The stubbed seams and the per-case runner ---------------------------

# The real architecture probe, kept before its stub replaces it, for the
# case script's platform check.
$realHostArch = ${function:Get-RelateHostArch}

$script:Served = @{}
$script:Calls = New-Object System.Collections.Generic.List[string]
$script:PathAdds = New-Object System.Collections.Generic.List[string]
function Invoke-RelateDownload {
    param([string] $Url, [string] $Destination)
    $script:Calls.Add($Url)
    if (-not $script:Served.ContainsKey($Url)) {
        Stop-RelateInstall (Get-RelateStatusMessage -Url $Url -Status 404)
    }
    Copy-Item -LiteralPath $script:Served[$Url] -Destination $Destination
}
function Get-RelateHostArch { return 'amd64' }
function Add-RelateUserPath {
    param([string] $Directory)
    $script:PathAdds.Add($Directory)
    return $true
}

# The default release: the metadata document and the three assets at
# the canonical URLs, overridable per case.
function Get-DefaultRelease {
    param([string] $Base = $RelateDownloadBase, [string] $ReleaseTag = $tag, [hashtable] $Manifest = $good,
        [string] $BinaryPath = $binary, [string] $ReleasesDoc = $releases,
        [string] $MetadataUrl = $RelateReleasesJsonUrl)
    return @{
        $MetadataUrl = $ReleasesDoc
        "$Base/$ReleaseTag/SHA256SUMS" = $Manifest.Sums
        "$Base/$ReleaseTag/SHA256SUMS.sig" = $Manifest.Sig
        "$Base/$ReleaseTag/$asset" = $BinaryPath
    }
}

$script:CaseNum = 0
# Run Install-Relate against $Serve with the env overrides in $Overrides; the
# result carries the thrown message (or $null), the captured output, the
# requested URLs and the install directory.
function Invoke-InstallCase {
    param([hashtable] $Serve, [hashtable] $Overrides = @{})
    $script:CaseNum++
    $script:Served = $Serve
    $script:Calls.Clear()
    $script:PathAdds.Clear()
    # The case's environment, one table: a fresh per-case LOCALAPPDATA, both
    # seams unset, then the case's overrides (a LOCALAPPDATA override reuses
    # an earlier case's root). It drives the snapshot, the set and the
    # restore alike, with Path snapshotted too: a successful install
    # prepends its directory to this session's Path.
    $caseEnv = @{
        LOCALAPPDATA = (Join-Path $Sandbox ("case-$script:CaseNum"))
        RELATE_RELEASES_JSON_URL = $null
        RELATE_RELEASE_DOWNLOAD_URL = $null
    }
    foreach ($name in $Overrides.Keys) { $caseEnv[$name] = $Overrides[$name] }
    $saved = @{}
    foreach ($name in @($caseEnv.Keys) + 'Path') { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
    foreach ($name in $caseEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $caseEnv[$name]) }
    $thrown = $null
    $records = @()
    try {
        # A precondition of the case, not an outcome: the installer's own
        # directory rule under the case's LOCALAPPDATA, resolved before the
        # recorded run, so an unresolvable one fails the harness with the
        # installer's authored message.
        $dir = Get-RelateInstallDir
        try {
            $records = @(Install-Relate *>&1)
        } catch {
            $thrown = $_.Exception.Message
        }
    } finally {
        foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
    }
    return @{
        LocalAppData = $caseEnv['LOCALAPPDATA']
        Error = $thrown
        # Each record's own text, joined: Out-String would wrap long lines
        # (the installed path) at the host width.
        Output = (@($records | ForEach-Object { "$_" }) -join "`n")
        # The warning stream on its own, so a case that claims a warning
        # asserts on warnings, not on any output line.
        Warnings = @($records | Where-Object { $_ -is [System.Management.Automation.WarningRecord] } |
            ForEach-Object { $_.Message })
        Calls = @($script:Calls)
        Dir = $dir
        Exe = (Join-Path $dir $RelateBinName)
        PathAdds = @($script:PathAdds)
    }
}

# One check over a run that must succeed: no error, and $Passed. A failed
# run names its error in the description.
function Test-Succeeded {
    param([string] $Description, [hashtable] $Run, [bool] $Passed = $true)
    $suffix = ''
    if ($Run.Error) { $suffix = " (got: $($Run.Error))" }
    Test-Check "$Description$suffix" ($null -eq $Run.Error -and $Passed)
}

# Run $Body with the function $Name replaced by $Stub for this call only.
# The stub is defined in this call's own scope: every function $Body calls
# resolves through it first (PowerShell's dynamic scoping), and it vanishes
# when the call returns, so there is no restore step to skip.
function Invoke-WithStub {
    param([string] $Name, [scriptblock] $Stub, [scriptblock] $Body)
    . ([ScriptBlock]::Create("function $Name {$Stub}"))
    & $Body
}

function Test-Installed {
    param([hashtable] $Run, [string] $Expected)
    if (-not (Test-Path -LiteralPath $Run.Exe)) { return $false }
    return ((Get-FileHash -LiteralPath $Run.Exe).Hash -eq (Get-FileHash -LiteralPath $Expected).Hash)
}

# True when $Dir holds exactly one entry: the installed binary, with no
# staged or retired copy beside it.
function Test-OnlyBinaryIn {
    param([string] $Dir)
    return ((Test-Path -LiteralPath $Dir) -and @(Get-ChildItem -LiteralPath $Dir -Force).Count -eq 1)
}

# The binary-asset requests a run made.
function Get-BinaryCalls {
    param([hashtable] $Run)
    return @($Run.Calls | Where-Object { $_.EndsWith("/$asset") })
}

# Refused before the binary: the installer threw a message carrying
# $Needle, installed nothing, and never paid for the binary download. The
# one refusal after the download, a checksum mismatch, asserts its own shape.
function Test-Refused {
    param([string] $Label, [hashtable] $Run, [string] $Needle)
    Test-Check "${Label}: the installer refuses" ($null -ne $Run.Error)
    Test-Check "${Label}: the error names the cause" ($null -ne $Run.Error -and $Run.Error.Contains($Needle))
    Test-Check "${Label}: nothing is installed" (-not (Test-Path -LiteralPath $Run.Exe))
    Test-Check "${Label}: the binary download never started" ((Get-BinaryCalls $Run).Count -eq 0)
}
