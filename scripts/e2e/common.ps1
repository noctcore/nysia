<#
    Shared helpers for the v0.1 end-to-end scripts. Dot-source it; it defines functions and
    does nothing on its own.

    The one thing worth knowing before reading further: **nothing here redirects a native
    command's stderr inline.** Windows PowerShell 5.1 wraps every stderr line from an
    executable in an ErrorRecord when you write `2>$null` or `2>&1`, which sets `$?` to
    false and — under `$ErrorActionPreference = 'Stop'` — terminates the script on a command
    that exited 0. Every `nysia` invocation therefore goes through `Invoke-Nysia`, which
    spawns it with the two streams redirected to files by the *operating system* and reads
    them back. That also keeps the console clean during the readiness poll, where failures
    are the expected case rather than a problem.
#>

# Dot-sourcing runs in the caller's scope, so this file deliberately sets no strict mode and
# no error preference: both belong to the script that sources it, and quietly changing them
# under an interactive shell is a surprise nobody asked for.

# The lease the daemon writes beside its endpoint. §12 question 5: its presence is never
# proof of life, but it is how a tool that did not spawn the daemon finds its pid.
#
# The version in the name is §3.1's mechanism, not decoration: it moves when the wire changes
# shape, and a daemon of the old version goes on serving its own endpoint beside the new one.
# It is spelled out here because PowerShell cannot read the Rust constant — keep it in step
# with `PROTOCOL_VERSION` in `crates/nysia-proto/src/version.rs`, which is what names the
# socket, the lease, the lock and the log.
$script:LeaseFile = 'nysiad-v2.pid.json'

<#
.SYNOPSIS
    Run one `nysia` verb and return its exit code and both streams, captured cleanly.
#>
function Invoke-Nysia {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string]   $Nysia,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    $outFile = [System.IO.Path]::GetTempFileName()
    $errFile = [System.IO.Path]::GetTempFileName()
    try {
        $process = Start-Process -FilePath $Nysia -ArgumentList $Arguments `
            -NoNewWindow -Wait -PassThru `
            -RedirectStandardOutput $outFile -RedirectStandardError $errFile
        $out = Get-Content $outFile -Raw
        $err = Get-Content $errFile -Raw
        return [pscustomobject] @{
            ExitCode = $process.ExitCode
            StdOut   = if ($null -eq $out) { '' } else { $out }
            StdErr   = if ($null -eq $err) { '' } else { $err }
        }
    }
    finally {
        Remove-Item $outFile, $errFile -Force -ErrorAction SilentlyContinue
    }
}

<#
.SYNOPSIS
    Run one `nysia` verb, insisting it succeeded, and return its stdout.
#>
function Invoke-NysiaOk {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string]   $Nysia,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    $run = Invoke-Nysia -Nysia $Nysia -Arguments $Arguments
    if ($run.ExitCode -ne 0) {
        throw "nysia $($Arguments -join ' ') exited $($run.ExitCode): $($run.StdErr)"
    }
    return $run.StdOut
}

<#
.SYNOPSIS
    Start a daemon on its own runtime directory and wait until it answers a verb.

.DESCRIPTION
    Isolated rather than convenient: on Windows the pipe namespace is machine-global, so the
    runtime directory has to reach the pipe *name* — which it does, and which is what keeps
    a script like this from binding over the daemon somebody is using.

    `--no-idle-retire`, because a script's own pauses are long enough to look like an idle
    daemon, and a measurement of a process that retired mid-run is not a measurement.
#>
function Start-NysiaDaemon {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string] $Nysia,
        [Parameter(Mandatory)] [string] $RuntimeDir,
        [int] $TimeoutSeconds = 45
    )

    Remove-Item -Recurse -Force $RuntimeDir -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $RuntimeDir | Out-Null
    $env:NYSIA_RUNTIME_DIR = $RuntimeDir

    Start-Process -FilePath $Nysia -ArgumentList '--daemon', '--no-idle-retire' -WindowStyle Hidden | Out-Null

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $run = Invoke-Nysia -Nysia $Nysia -Arguments @('session', 'list', '--no-spawn')
        if ($run.ExitCode -eq 0) {
            return Get-NysiaDaemonPid -RuntimeDir $RuntimeDir
        }
        Start-Sleep -Milliseconds 200
    }
    throw "the daemon never began answering on $RuntimeDir"
}

<#
.SYNOPSIS
    The pid in the lease beside the endpoint, or 0 when there is no lease.
#>
function Get-NysiaDaemonPid {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $RuntimeDir)

    $lease = Join-Path $RuntimeDir $script:LeaseFile
    if (-not (Test-Path $lease)) { return 0 }
    return [int] ((Get-Content $lease -Raw | ConvertFrom-Json).pid)
}

<#
.SYNOPSIS
    Kill a daemon and everything it spawned, then remove its runtime directory.

.DESCRIPTION
    `/T`, because the shells are the daemon's children: a bare kill orphans them, and an
    orphaned shell is a process the *next* run counts.
#>
function Stop-NysiaDaemon {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [int] $DaemonPid,
        [string] $RuntimeDir = ''
    )

    if ($DaemonPid -gt 0) {
        # taskkill /T, not Stop-Process: the shells are the daemon's children, a bare kill
        # orphans them, and an orphaned shell is a process the *next* run counts.
        $out = [System.IO.Path]::GetTempFileName()
        $err = [System.IO.Path]::GetTempFileName()
        try {
            Start-Process -FilePath 'taskkill' -ArgumentList '/PID', $DaemonPid, '/T', '/F' `
                -NoNewWindow -Wait -RedirectStandardOutput $out -RedirectStandardError $err `
                -ErrorAction SilentlyContinue
        }
        finally {
            Remove-Item $out, $err -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 400
    }
    if ($RuntimeDir -ne '') {
        Remove-Item -Recurse -Force $RuntimeDir -ErrorAction SilentlyContinue
    }
}

<#
.SYNOPSIS
    Every descendant of a process, not only its children.

.DESCRIPTION
    A ConPTY session brings a `conhost.exe` as well as the shell, and the shell spawns
    processes of its own. A one-level walk would leave them out of a total that claims to be
    what the machine pays.
#>
function Get-NysiaDescendants {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [int] $Root)

    $byParent = @{}
    foreach ($process in Get-CimInstance Win32_Process) {
        $parent = [int] $process.ParentProcessId
        if (-not $byParent.ContainsKey($parent)) { $byParent[$parent] = @() }
        $byParent[$parent] += $process
    }

    $found = @()
    $queue = [System.Collections.Queue]::new()
    $queue.Enqueue([int] $Root)
    while ($queue.Count -gt 0) {
        $parent = [int] $queue.Dequeue()
        if (-not $byParent.ContainsKey($parent)) { continue }
        foreach ($child in $byParent[$parent]) {
            $found += $child
            $queue.Enqueue([int] $child.ProcessId)
        }
    }
    return $found
}

<#
.SYNOPSIS
    One process's memory, in both of the numbers that mean something.

.DESCRIPTION
    Working set is what Task Manager shows and what the §12 comparison figures were read
    off; private bytes is what the process would still cost if nothing were shared. Neither
    alone is the honest answer, so both are reported everywhere.
#>
function Measure-NysiaProcess {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [int] $ProcessId)

    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if (-not $process) { return $null }
    return [pscustomobject] @{
        ProcessId    = $process.Id
        Name         = $process.ProcessName
        WorkingSetMB = [math]::Round($process.WorkingSet64 / 1MB, 1)
        PrivateMB    = [math]::Round($process.PrivateMemorySize64 / 1MB, 1)
    }
}

<#
.SYNOPSIS
    A process and every descendant, summed.
#>
function Measure-NysiaTree {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [int] $Root,
        [string] $Label = ''
    )

    # Not `$root`: PowerShell variable names are case-insensitive, so a local by that name is
    # the `$Root` parameter, and the recursive walk below would be handed the measurement
    # instead of the pid.
    $rootProcess = Measure-NysiaProcess -ProcessId $Root
    if (-not $rootProcess) { throw "process $Root is not running, so there is nothing to measure" }

    $children = @(
        Get-NysiaDescendants -Root $Root |
            ForEach-Object { Measure-NysiaProcess -ProcessId ([int] $_.ProcessId) } |
            Where-Object { $null -ne $_ }
    )
    $childWorkingSet = [double] (($children | Measure-Object -Property WorkingSetMB -Sum).Sum)
    $childPrivate = [double] (($children | Measure-Object -Property PrivateMB -Sum).Sum)

    return [pscustomobject] @{
        Label                = $Label
        RootProcessId        = $rootProcess.ProcessId
        RootName             = $rootProcess.Name
        RootWorkingSetMB     = $rootProcess.WorkingSetMB
        RootPrivateMB        = $rootProcess.PrivateMB
        ChildProcesses       = $children.Count
        ChildrenWorkingSetMB = [math]::Round($childWorkingSet, 1)
        ChildrenPrivateMB    = [math]::Round($childPrivate, 1)
        TotalWorkingSetMB    = [math]::Round($rootProcess.WorkingSetMB + $childWorkingSet, 1)
        TotalPrivateMB       = [math]::Round($rootProcess.PrivateMB + $childPrivate, 1)
        Children             = $children
    }
}
