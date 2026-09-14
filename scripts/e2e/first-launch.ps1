<#
.SYNOPSIS
    A first launch on a machine with no daemon: the app starts one and the tab works.

.DESCRIPTION
    §12 q6 as a script. `state::tests::a_window_with_no_daemon_anywhere_starts_one_and_reaches_a_working_session`
    proves the window's client starts a daemon and reaches a session, and the `bundle` job in
    CI proves the app that ships contains the `nysia` runtime to start. Neither one launches
    the bundled app, and the two halves are exactly where this defect lived: the code could
    not spawn, and the bundle carried nothing to spawn.

    What only a real launch can show:

    - the sidecar is found **where Tauri actually put it**, which is beside the installed
      executable rather than beside a `cargo` output directory;
    - a window that was given no daemon reaches *Ready* rather than *Reconnecting*;
    - the `+` menu opens a tab instead of failing with "Start the Nysia daemon, then try
      again";
    - and, in `-Step missing`, that an app whose runtime has been taken away says so in a
      sentence a person can act on, once, rather than reconnecting for ever in silence.

    **The last one needs your eyes.** A notice on screen is not something a script can read,
    so that step sets the situation up, tells you what the window must say, and asks.

    **It does not touch the daemon you are using.** Everything runs under a runtime directory
    of its own, which on Windows reaches the pipe name as well as the directory.

        powershell -File scripts/e2e/first-launch.ps1 -Step launch -App <installed Nysia.exe>
        #   ... open a tab from the + menu and run the lines it prints ...
        powershell -File scripts/e2e/first-launch.ps1 -Step typed
        powershell -File scripts/e2e/first-launch.ps1 -Step missing
        powershell -File scripts/e2e/first-launch.ps1 -Step finish

.PARAMETER Step
    Which step to run. See the description.

.PARAMETER App
    The **installed** window, with its sidecar beside it — the executable inside the directory
    the NSIS installer wrote, or `target/release/nysia-desktop.exe` after `tauri build`, which
    is the same layout. A `cargo build` output works too and proves less: `nysia.exe` is in
    that directory whether or not anything bundled it.

.PARAMETER Nysia
    A `nysia` binary for the script's own verbs. It is **not** the one under test: the app
    starts its own, and this one only asks the daemon questions afterwards.

.PARAMETER RuntimeDir
    Where the endpoint, the lease and this run's state live.

.PARAMETER Shell
    Which launcher to open in the `+` menu. Pick the one this machine actually has.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('launch', 'typed', 'missing', 'finish')]
    [string] $Step,
    [string] $App = (Join-Path $PSScriptRoot '..\..\target\release\nysia-desktop.exe'),
    [string] $Nysia = (Join-Path $PSScriptRoot '..\..\target\release\nysia.exe'),
    [string] $RuntimeDir = (Join-Path $env:TEMP 'nysia-first-launch'),
    [ValidateSet('pwsh', 'cmd', 'git_bash')]
    [string] $Shell = 'pwsh'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

# Computed by the shell, never typed — the same reasoning as the walking skeleton's: a token
# that also appears in the line as typed is satisfied by kernel echo with the shell having
# run nothing at all.
$script:Token = 'NYSIA-42'
$script:Recipe = @{
    pwsh     = @('Write-Output ("NYSIA" + "-" + (6*7))')
    cmd      = @('set /a NYS=6*7', 'echo NYSIA-%NYS%')
    git_bash = @('echo "NYSIA-$((6*7))"')
}

$script:StatePath = Join-Path $RuntimeDir 'first-launch.state.json'
$script:Failed = $false

function Save-State {
    param([Parameter(Mandatory)] [hashtable] $State)
    $State | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $script:StatePath
}

function Read-State {
    param([Parameter(Mandatory)] [string[]] $Expected)

    if (-not (Test-Path $script:StatePath)) {
        throw "No run in progress under $RuntimeDir. Start one with -Step launch."
    }
    $state = @{}
    (Get-Content $script:StatePath -Raw | ConvertFrom-Json).PSObject.Properties |
        ForEach-Object { $state[$_.Name] = $_.Value }
    if ($state['step'] -notin $Expected) {
        throw "This run is at step '$($state['step'])', so -Step $Step is out of order."
    }
    return $state
}

function Write-Check {
    param([string] $Claim, [bool] $Held, [string] $Detail = '')

    $mark = if ($Held) { 'PASS' } else { 'FAIL' }
    $colour = if ($Held) { 'Green' } else { 'Red' }
    Write-Host ("  [{0}] {1}" -f $mark, $Claim) -ForegroundColor $colour
    if ($Detail -ne '') { Write-Host "         $Detail" -ForegroundColor DarkGray }
    if (-not $Held) { $script:Failed = $true }
}

function Assert-NoFailures {
    if ($script:Failed) { throw 'a check failed; see the FAIL lines above' }
}

# `,@(...)` on the return: PowerShell unwraps a one-element array as it leaves a function, and
# `Set-StrictMode -Version Latest` then makes `.Count` on the scalar a terminating error — so a
# run with exactly one session, which is every run this script is for, would die on the first
# check rather than report it.
function Get-SessionRow {
    $rows = @(Invoke-NysiaOk -Nysia $Nysia -Arguments @('session', 'list', '--json', '--no-spawn') | ConvertFrom-Json)
    return ,$rows
}

# The runtime beside the window, which is the file the app starts. Named here rather than
# assumed, because the whole question this script asks is whether it is there.
function Get-Sidecar {
    param([string] $Window)
    return (Join-Path (Split-Path -Parent $Window) 'nysia.exe')
}

function Start-App {
    param([string] $Binary)

    $stdout = Join-Path $RuntimeDir 'app.out.log'
    $stderr = Join-Path $RuntimeDir 'app.err.log'
    $env:NYSIA_RUNTIME_DIR = $RuntimeDir
    $process = Start-Process -FilePath $Binary -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # The window has to come up, resolve its sidecar, spawn a daemon and wait for it to bind.
    # Longer than the walking skeleton's pause for exactly that reason.
    Start-Sleep -Seconds 12
    return [int] $process.Id
}

$App = (Resolve-Path $App).Path
$Nysia = (Resolve-Path $Nysia).Path

switch ($Step) {

    'launch' {
        Write-Host '== step 1/4: launch the app with no daemon anywhere ==' -ForegroundColor Cyan

        Remove-Item -Recurse -Force $RuntimeDir -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Force $RuntimeDir | Out-Null
        $env:NYSIA_RUNTIME_DIR = $RuntimeDir

        $sidecar = Get-Sidecar -Window $App
        Write-Check 'the app ships the nysia runtime beside it' (Test-Path $sidecar) $sidecar

        # The premise, asserted rather than assumed: if something is already listening here,
        # everything below would pass without the app having started anything.
        $before = Invoke-Nysia -Nysia $Nysia -Arguments @('session', 'list', '--no-spawn')
        Write-Check 'nothing is listening before the app starts' ($before.ExitCode -ne 0) $before.StdErr
        Assert-NoFailures

        $appPid = Start-App -Binary $App
        $daemonPid = Get-NysiaDaemonPid -RuntimeDir $RuntimeDir

        Write-Check 'the app is running' ((Get-Process -Id $appPid -ErrorAction SilentlyContinue) -ne $null) "pid $appPid"
        Write-Check 'a daemon is now listening' ($daemonPid -gt 0) "pid $daemonPid"
        if ($daemonPid -gt 0) {
            $after = Invoke-Nysia -Nysia $Nysia -Arguments @('session', 'list', '--no-spawn')
            Write-Check 'and it answers verbs' ($after.ExitCode -eq 0) $after.StdErr
            # Started *by the window*, not left behind by something else: the daemon is
            # detached, so it is not a child of the app, but it is the app's binary that ran.
            $image = (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue).Path
            Write-Check 'it is the runtime that ships with the app' ($image -eq $sidecar) $image
        }

        Save-State @{
            step       = 'launch'
            runtimeDir = $RuntimeDir
            app        = $App
            appPid     = $appPid
            daemonPid  = $daemonPid
            sidecar    = $sidecar
            shell      = $Shell
        }
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW DO THIS, in the window that just opened:' -ForegroundColor Yellow
        Write-Host '  1. The status bar must read Ready, not Reconnecting.'
        Write-Host "  2. Click + in the title bar and choose the $Shell launcher. It must open a"
        Write-Host '     tab, not fail with "Start the Nysia daemon, then try again".'
        Write-Host '  3. Click into the terminal and run these lines, one at a time:'
        foreach ($line in $script:Recipe[$Shell]) { Write-Host "       $line" -ForegroundColor White }
        Write-Host '  4. Then run: -Step typed'
    }

    'typed' {
        Write-Host '== step 2/4: the tab the app opened is a working session ==' -ForegroundColor Cyan
        $state = Read-State -Expected @('launch')
        $env:NYSIA_RUNTIME_DIR = $state['runtimeDir']

        $rows = Get-SessionRow
        Write-Check 'the daemon holds the session the window opened' ($rows.Count -ge 1) "$($rows.Count) session(s)"
        Assert-NoFailures

        $handle = $rows[0].handle
        $screen = Invoke-NysiaOk -Nysia $Nysia -Arguments @('terminal', 'read', $handle, '--screen', '--no-spawn')
        Write-Check "the shell computed $($script:Token)" ($screen -match $script:Token) `
            'a screen showing it cannot be showing an echo of what was typed'

        $state['step'] = 'typed'
        $state['handle'] = $handle
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW DO THIS:' -ForegroundColor Yellow
        Write-Host '  1. Close the window with the X in its title bar.'
        Write-Host '  2. Then run: -Step missing'
    }

    'missing' {
        # The other half of "truthful, not a permanent Reconnecting". A window that cannot
        # start a runtime has to say so — the failure is not retryable, so the store shows the
        # notice and stops rather than spinning against something no amount of waiting fixes.
        Write-Host '== step 3/4: an app whose runtime is missing says so ==' -ForegroundColor Cyan
        $state = Read-State -Expected @('typed')
        $env:NYSIA_RUNTIME_DIR = $state['runtimeDir']

        # `-RuntimeDir` as well as the pid: it removes the runtime directory, and with it the
        # lease. Killing the process alone leaves the lease behind, `Get-NysiaDaemonPid` reads
        # the dead pid out of it, and the check below reports a daemon that is not there.
        Stop-NysiaDaemon -DaemonPid ([int] $state['daemonPid']) -RuntimeDir $state['runtimeDir'] | Out-Null
        New-Item -ItemType Directory -Force $state['runtimeDir'] | Out-Null

        $sidecar = [string] $state['sidecar']
        $hidden = "$sidecar.moved"
        Move-Item -Force $sidecar $hidden
        Write-Check 'the runtime has been moved out of the way' (-not (Test-Path $sidecar)) $hidden

        $appPid = Start-App -Binary ([string] $state['app'])
        Write-Check 'no daemon was started' ((Get-NysiaDaemonPid -RuntimeDir $state['runtimeDir']) -eq 0)

        $state['step'] = 'missing'
        $state['appPid'] = $appPid
        $state['hiddenSidecar'] = $hidden
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'LOOK AT THE WINDOW. It must say all of this:' -ForegroundColor Yellow
        Write-Host '  - that the Nysia runtime is not installed beside the app, naming the path;'
        Write-Host '  - a next step: reinstall, or run `nysia --daemon` yourself;'
        Write-Host '  - and it must settle there. A status bar still cycling Reconnecting a'
        Write-Host '    minute later is the defect, not the fix.'
        Write-Host '  Then run: -Step finish'
    }

    'finish' {
        Write-Host '== step 4/4: put everything back ==' -ForegroundColor Cyan
        $state = Read-State -Expected @('launch', 'typed', 'missing')
        $env:NYSIA_RUNTIME_DIR = $state['runtimeDir']

        foreach ($name in @('appPid')) {
            $id = [int] $state[$name]
            if ($id -gt 0) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
        }
        if ($state.ContainsKey('hiddenSidecar') -and (Test-Path $state['hiddenSidecar'])) {
            Move-Item -Force $state['hiddenSidecar'] $state['sidecar']
            Write-Check 'the runtime is back beside the app' (Test-Path $state['sidecar']) $state['sidecar']
        }
        $daemonPid = [int] $state['daemonPid']
        Stop-NysiaDaemon -DaemonPid $daemonPid -RuntimeDir $state['runtimeDir'] | Out-Null
        # The process, not the lease: `Stop-NysiaDaemon` has just removed the directory the
        # lease lived in, so asking for it again would answer 0 whatever happened.
        Write-Check 'the daemon this run started is gone' `
            ($daemonPid -eq 0 -or (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue) -eq $null)
        Assert-NoFailures
        Write-Host 'done.' -ForegroundColor Green
    }
}
