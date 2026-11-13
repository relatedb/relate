# The TAP runtime for the PowerShell test harnesses: the twin of
# scripts/tests/lib/tap.sh. Two kits: the TAP output itself (Test-Check's
# numbered ok/not ok lines, Exit-TapRun's plan and summary footer and exit
# status, 0 only when nothing failed; the counters live here, once), and the
# throw assertions (Get-Thrown, Test-NoThrow, Test-Throws). Dot-source it:
#   . (Join-Path (Join-Path $PSScriptRoot 'lib') 'tap.ps1')

$script:TapNum = 0
$script:TapFailed = 0
function Test-Check {
    param([string] $Description, [bool] $Passed)
    $script:TapNum++
    if ($Passed) {
        Write-Output "ok $script:TapNum - $Description"
    } else {
        $script:TapFailed++
        Write-Output "not ok $script:TapNum - $Description"
    }
}

# The plan and summary footer, then the script's exit: status 0 only when
# nothing failed (`exit` in a function ends the calling script).
function Exit-TapRun {
    Write-Output "1..$script:TapNum"
    Write-Output "# pass $($script:TapNum - $script:TapFailed), fail $script:TapFailed (of $script:TapNum)"
    if ($script:TapFailed -ne 0) { exit 1 }
    exit 0
}

# The message a script block throws, or $null when it completes.
function Get-Thrown {
    param([scriptblock] $Block)
    try { & $Block 3>$null | Out-Null; return $null } catch { return $_.Exception.Message }
}

# One check: $Block completes without throwing.
function Test-NoThrow {
    param([string] $Description, [scriptblock] $Block)
    Test-Check $Description ($null -eq (Get-Thrown $Block))
}

# One check: $Block throws, with a message carrying $Needle.
function Test-Throws {
    param([string] $Description, [scriptblock] $Block, [string] $Needle)
    $thrown = Get-Thrown $Block
    Test-Check $Description ($null -ne $thrown -and $thrown.Contains($Needle))
}
