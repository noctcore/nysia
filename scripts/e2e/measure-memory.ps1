<#
.SYNOPSIS
    Measure what a Nysia daemon costs, per session, so §12 question 3 can stop guessing.

.DESCRIPTION
    Architecture §12 records Orca's figures on the author's machine — app 1.1 GB, 356-817 MB
    per worktree, PTY daemon 88 MB for five terminals — and asks what Nysia's honest claim
    is. Guessing is what this script exists to replace.

    For each session count it starts a *fresh* daemon on its own runtime directory, opens
    that many shells, waits for every one of them to go idle, and then measures. Fresh,
    because a daemon that has already held ten sessions has grown allocator arenas a daemon
    holding one never had, and reporting the second number as the first flatters the result.

    **The children are counted.** On Windows every ConPTY session brings a `conhost.exe` as
    well as the shell, and a daemon RSS that excluded them would measure the bookkeeping
    rather than the cost. Both totals are printed: the daemon alone, which is what Orca's
    88 MB comparator is, and the daemon with everything it spawned, which is what the
    machine actually pays.

.PARAMETER Nysia
    The `nysia` binary. Defaults to the release build in this worktree.

.PARAMETER Counts
    The session counts to measure.

.PARAMETER Shell
    The shell profile to open. `pwsh` where PowerShell 7 is installed, `cmd` where it is not.

.PARAMETER Json
    Also write the table to this path as JSON.

.EXAMPLE
    powershell -File scripts/e2e/measure-memory.ps1 -Shell cmd
#>
[CmdletBinding()]
param(
    [string] $Nysia = (Join-Path $PSScriptRoot '..\..\target\release\nysia.exe'),
    [int[]]  $Counts = @(1, 5, 10),
    [ValidateSet('pwsh', 'cmd', 'git_bash')]
    [string] $Shell = 'pwsh',
    [string] $Json = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

if (-not (Test-Path $Nysia)) {
    throw "No nysia binary at $Nysia. Build one with cwd = the crate directory: cd crates/nysia; cargo build --release"
}
$Nysia = (Resolve-Path $Nysia).Path

$results = @()

foreach ($count in $Counts) {
    $runtimeDir = Join-Path $env:TEMP "nysia-measure-$count-$PID"
    Write-Host "== $count session(s), profile $Shell ==" -ForegroundColor Cyan
    $daemonPid = Start-NysiaDaemon -Nysia $Nysia -RuntimeDir $runtimeDir
    try {
        $handles = @()
        for ($i = 0; $i -lt $count; $i++) {
            $created = Invoke-NysiaOk -Nysia $Nysia -Arguments @(
                'session', 'create', '--json', '--no-spawn', '--profile', $Shell
            ) | ConvertFrom-Json
            $handles += $created.handle
        }

        # Measured after every shell has drawn its prompt and gone quiet. A daemon measured
        # the instant after a spawn is measured mid-startup, and that number is neither the
        # steady state nor the peak.
        foreach ($handle in $handles) {
            Invoke-Nysia -Nysia $Nysia -Arguments @(
                'terminal', 'wait', $handle, '--for', 'idle', '--timeout-ms', '20000', '--no-spawn'
            ) | Out-Null
        }
        Start-Sleep -Seconds 2

        $tree = Measure-NysiaTree -Root $daemonPid -Label "$count sessions"
        $results += [pscustomobject] @{
            Sessions             = $count
            DaemonWorkingSetMB   = $tree.RootWorkingSetMB
            DaemonPrivateMB      = $tree.RootPrivateMB
            ChildProcesses       = $tree.ChildProcesses
            ChildrenWorkingSetMB = $tree.ChildrenWorkingSetMB
            ChildrenPrivateMB    = $tree.ChildrenPrivateMB
            TotalWorkingSetMB    = $tree.TotalWorkingSetMB
            TotalPrivateMB       = $tree.TotalPrivateMB
        }
        $tree.Children | Format-Table -AutoSize | Out-String | Write-Host
    }
    finally {
        Stop-NysiaDaemon -DaemonPid $daemonPid -RuntimeDir $runtimeDir
    }
}

$results | Format-Table -AutoSize
if ($Json -ne '') {
    $results | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $Json
    Write-Host "wrote $Json"
}
