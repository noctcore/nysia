<#
.SYNOPSIS
    The v0.1 acceptance criterion, driven against the real GUI: kill the UI and the shell
    survives; reattach and the scrollback replays.

.DESCRIPTION
    `crates/nysia/tests/survival.rs` proves the daemon half with no GUI anywhere near it, and
    `interop::a_relaunched_window_finds_the_session_and_replays_its_scrollback` proves the
    window's client half in process. Neither one launches the app. This does — and the three
    things it can only get from a real launch are a real process exit, a genuinely new client
    id, and a webview that actually paints.

    **Why it is stepped rather than one run.** Opening a tab and closing a window are things a
    person does, and a script that pretended otherwise would be synthesising input and calling
    it a GUI test. So each step is a separate invocation: the script does everything a machine
    can do, checks what it can check, and then tells you exactly what to do before the next
    step. State carries between invocations in a JSON file beside the endpoint.

    Run them in order. Every step re-reads the state file, so a step run out of order says so
    rather than reporting on a session that is not the one it set up.

        powershell -File scripts/e2e/walking-skeleton.ps1 -Step start
        #   ... open a terminal tab and run the two lines it prints ...
        powershell -File scripts/e2e/walking-skeleton.ps1 -Step typed
        #   ... close the window with the X in its title bar ...
        powershell -File scripts/e2e/walking-skeleton.ps1 -Step closed
        powershell -File scripts/e2e/walking-skeleton.ps1 -Step relaunched
        powershell -File scripts/e2e/walking-skeleton.ps1 -Step upgraded -NewAppBinary <path>
        powershell -File scripts/e2e/walking-skeleton.ps1 -Step finish

    **It does not touch the daemon you are using.** Everything runs under a runtime directory
    of its own, which on Windows reaches the pipe name as well as the directory, so the app it
    launches finds this daemon and no other.

.PARAMETER Step
    Which step to run. See the description.

.PARAMETER Nysia
    The `nysia` binary. Defaults to the release build in this worktree.

.PARAMETER App
    The desktop binary. Defaults to the release build in this worktree.

    **Build it with `--features custom-protocol`** (or through `pnpm build:app`). Without that
    feature the window loads `devUrl` instead of the bundled frontend, and a release build with
    no vite server behind it shows a WebView2 connection error rather than Nysia.

.PARAMETER NewAppBinary
    For `-Step upgraded`: the replacement app binary, which must differ from the running one.

.PARAMETER RuntimeDir
    Where the daemon's endpoint and this run's state live.

.PARAMETER Shell
    Which launcher to open in the `+` menu. Pick the one this machine actually has.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('start', 'typed', 'closed', 'relaunched', 'upgraded', 'finish')]
    [string] $Step,
    [string] $Nysia = (Join-Path $PSScriptRoot '..\..\target\release\nysia.exe'),
    [string] $App = (Join-Path $PSScriptRoot '..\..\target\release\nysia-desktop.exe'),
    [string] $NewAppBinary = '',
    [string] $RuntimeDir = (Join-Path $env:TEMP 'nysia-walking-skeleton'),
    [ValidateSet('pwsh', 'cmd', 'git_bash')]
    [string] $Shell = 'pwsh'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'common.ps1')

# Computed by the shell, never typed. A check that looks for a string which also appears in
# the line as typed is satisfied by kernel echo, with the shell having run nothing — and after
# a relaunch it is satisfied a second time by a replay of that echo, which is the very thing
# under test.
$script:Token = 'NYSIA-42'
$script:Recipe = @{
    pwsh     = @('Write-Output ("NYSIA" + "-" + (6*7))')
    # `cmd` expands %NYS% when it parses the line, so the assignment has to be its own command.
    cmd      = @('set /a NYS=6*7', 'echo NYSIA-%NYS%')
    git_bash = @('echo "NYSIA-$((6*7))"')
}

$script:StatePath = Join-Path $RuntimeDir 'walking-skeleton.state.json'

function Save-State {
    param([Parameter(Mandatory)] [hashtable] $State)
    $State | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $script:StatePath
}

function Read-State {
    param([Parameter(Mandatory)] [string] $Expected)

    if (-not (Test-Path $script:StatePath)) {
        throw "No run in progress under $RuntimeDir. Start one with -Step start."
    }
    $state = @{}
    (Get-Content $script:StatePath -Raw | ConvertFrom-Json).PSObject.Properties |
        ForEach-Object { $state[$_.Name] = $_.Value }
    if ($state['step'] -ne $Expected) {
        throw "This run is at step '$($state['step'])', so -Step $Step is out of order; the next step is the one after '$($state['step'])'."
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
    if ($script:Failed) {
        throw 'a check failed; see the FAIL lines above'
    }
}

# `,@(...)` on every return, here and in Get-ShellPids. PowerShell unwraps a one-element array
# as it leaves a function, and `Set-StrictMode -Version Latest` then makes `.Count` on the
# scalar a terminating error — so a run with exactly one session or one shell, which is every
# run this script is for, would die on the first check rather than report it.
function Get-SessionRow {
    param([string] $Handle = '')

    $rows = @(Invoke-NysiaOk -Nysia $Nysia -Arguments @('session', 'list', '--json', '--no-spawn') | ConvertFrom-Json)
    if ($Handle -ne '') { $rows = @($rows | Where-Object { $_.handle -eq $Handle }) }
    return ,$rows
}

# The shell the daemon spawned for this session, found through the process tree rather than
# through the daemon: the point of the whole exercise is that this process outlives the window,
# and asking the daemon whether it thinks it does would be asking the wrong witness.
function Get-ShellPids {
    param([int] $DaemonPid)

    $pids = @(
        Get-NysiaDescendants -Root $DaemonPid |
            Where-Object { $_.Name -notin @('conhost.exe') } |
            ForEach-Object { [int] $_.ProcessId } |
            Sort-Object
    )
    return ,$pids
}

function Start-App {
    param([string] $Binary)

    $stdout = Join-Path $RuntimeDir 'app.out.log'
    $stderr = Join-Path $RuntimeDir 'app.err.log'
    $env:NYSIA_RUNTIME_DIR = $RuntimeDir
    $process = Start-Process -FilePath $Binary -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    Start-Sleep -Seconds 8
    return [int] $process.Id
}

$Nysia = (Resolve-Path $Nysia).Path
$App = (Resolve-Path $App).Path
$script:Failed = $false

switch ($Step) {

    'start' {
        Write-Host '== step 1/6: start a daemon and launch the app ==' -ForegroundColor Cyan
        $daemonPid = Start-NysiaDaemon -Nysia $Nysia -RuntimeDir $RuntimeDir
        $lease = Get-Content (Join-Path $RuntimeDir $script:LeaseFile) -Raw | ConvertFrom-Json
        $appPid = Start-App -Binary $App

        Write-Check 'the daemon is listening' $true "pid $daemonPid, launch nonce $($lease.launchNonce)"
        Write-Check 'the app is running' ((Get-Process -Id $appPid -ErrorAction SilentlyContinue) -ne $null) "pid $appPid"
        Write-Check 'the daemon holds no sessions yet' ((Get-SessionRow).Count -eq 0)

        Save-State @{
            step         = 'start'
            runtimeDir   = $RuntimeDir
            daemonPid    = $daemonPid
            launchNonce  = $lease.launchNonce
            daemonStart  = $lease.startedAtMs
            appPid       = $appPid
            appBinary    = $App
            appSha256    = (Get-FileHash $App -Algorithm SHA256).Hash
            shell        = $Shell
        }
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW DO THIS, in the window that just opened:' -ForegroundColor Yellow
        Write-Host "  1. Click + in the title bar and choose the $Shell launcher."
        Write-Host '  2. Click into the terminal and run these lines, one at a time:'
        foreach ($line in $script:Recipe[$Shell]) { Write-Host "       $line" -ForegroundColor White }
        Write-Host "     The last one must print $($script:Token). It is computed, so a screen that"
        Write-Host '     shows it cannot be showing an echo of what you typed.'
        Write-Host '  3. Then run: -Step typed'
    }

    'typed' {
        Write-Host '== step 2/6: the session is live and the window painted it ==' -ForegroundColor Cyan
        $state = Read-State -Expected 'start'
        $env:NYSIA_RUNTIME_DIR = $RuntimeDir

        $rows = Get-SessionRow
        Write-Check 'the window opened exactly one session' ($rows.Count -eq 1) "the daemon holds $($rows.Count)"
        Assert-NoFailures
        $handle = $rows[0].handle

        $screen = Invoke-NysiaOk -Nysia $Nysia -Arguments @('terminal', 'read', $handle, '--screen', '--no-spawn')
        Write-Check "the shell computed $($script:Token)" ($screen -match [regex]::Escape($script:Token))
        $shellPids = Get-ShellPids -DaemonPid ([int] $state['daemonPid'])
        Write-Check 'the daemon owns a shell process' ($shellPids.Count -ge 1) "pids $($shellPids -join ', ')"

        Write-Host '--- the screen the daemon holds ---' -ForegroundColor DarkGray
        Write-Host $screen

        $state['step'] = 'typed'
        $state['handle'] = $handle
        $state['paneKey'] = $rows[0].paneKey
        $state['createdAtMs'] = $rows[0].createdAtMs
        $state['shellPids'] = $shellPids
        $state['screenBefore'] = $screen
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW DO THIS:' -ForegroundColor Yellow
        Write-Host '  Close the WINDOW with the X in its title bar. Do not stop the daemon.'
        Write-Host '  Then run: -Step closed'
    }

    'closed' {
        Write-Host '== step 3/6: the UI is gone and the shell is not ==' -ForegroundColor Cyan
        $state = Read-State -Expected 'typed'
        $env:NYSIA_RUNTIME_DIR = $RuntimeDir

        Write-Check 'the app process is gone' `
            ($null -eq (Get-Process -Id ([int] $state['appPid']) -ErrorAction SilentlyContinue))
        Assert-NoFailures

        # A pause long enough that a daemon which tore its sessions down with its last client
        # would have done so. Checking instantly would pass against one that was about to.
        Start-Sleep -Seconds 3

        $daemonPid = [int] $state['daemonPid']
        Write-Check 'the daemon is still running' ((Get-Process -Id $daemonPid -ErrorAction SilentlyContinue) -ne $null) "pid $daemonPid"

        $rows = Get-SessionRow -Handle $state['handle']
        Write-Check 'the session outlived the window' ($rows.Count -eq 1) $state['handle']

        $shellPids = Get-ShellPids -DaemonPid $daemonPid
        $same = (@(Compare-Object $shellPids @($state['shellPids'])).Count -eq 0)
        Write-Check 'the same shell process is still running' $same `
            "was $(@($state['shellPids']) -join ', '), now $($shellPids -join ', ')"

        $screen = Invoke-NysiaOk -Nysia $Nysia -Arguments @(
            'terminal', 'read', $state['handle'], '--screen', '--no-spawn'
        )
        Write-Check "the screen still shows $($script:Token)" ($screen -match [regex]::Escape($script:Token))

        $state['step'] = 'closed'
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'Next: -Step relaunched (the script launches the app again itself).' -ForegroundColor Yellow
    }

    'relaunched' {
        Write-Host '== step 4/6: relaunch, and the tab comes back with its scrollback ==' -ForegroundColor Cyan
        $state = Read-State -Expected 'closed'

        # The screen as it stands with no window attached to it. Everything between here and
        # the comparison below is the app attaching, and the app attaching must not type
        # anything: a replay carries every terminal query the child ever wrote, a renderer
        # answers a query when it parses one and cannot tell a replayed one from a live one,
        # and ConPTY reads the answer to a cursor-position report as F3 — which `cmd` treats as
        # recall-previous-command. This is the defect reported against v0.1, made into a check
        # rather than an instruction to look at the screen and notice.
        $beforeAttach = Invoke-NysiaOk -Nysia $Nysia -Arguments @('terminal', 'read', $state['handle'], '--screen', '--no-spawn')

        $appPid = Start-App -Binary $state['appBinary']

        $daemonPid = [int] $state['daemonPid']
        $lease = Get-Content (Join-Path $RuntimeDir $script:LeaseFile) -Raw | ConvertFrom-Json
        Write-Check 'the daemon never restarted' `
            ($lease.pid -eq $daemonPid -and $lease.launchNonce -eq $state['launchNonce']) `
            "pid $($lease.pid), launch nonce $($lease.launchNonce)"

        $rows = Get-SessionRow -Handle $state['handle']
        Write-Check 'the handle did not rotate' ($rows.Count -eq 1) $state['handle']
        if ($rows.Count -eq 1) {
            Write-Check 'the pane key did not rotate' ($rows[0].paneKey -eq $state['paneKey'])
            Write-Check 'it is the same session, not a new one' ($rows[0].createdAtMs -eq $state['createdAtMs']) `
                "created at $($rows[0].createdAtMs)"
        }

        $shellPids = Get-ShellPids -DaemonPid $daemonPid
        Write-Check 'the same shell process is still running' `
            ((@(Compare-Object $shellPids @($state['shellPids'])).Count -eq 0)) `
            "pids $($shellPids -join ', ')"

        # Settle before comparing. A window that injected input produces output — the recalled
        # line is echoed — so waiting for the session to go quiet is what makes the comparison
        # meaningful rather than a race the fix happens to win.
        $null = Invoke-Nysia -Nysia $Nysia -Arguments @('terminal', 'wait', $state['handle'], '--for', 'idle', '--timeout-ms', '20000', '--no-spawn')

        $screen = Invoke-NysiaOk -Nysia $Nysia -Arguments @('terminal', 'read', $state['handle'], '--screen', '--no-spawn')
        Write-Check "the scrollback still shows $($script:Token)" ($screen -match [regex]::Escape($script:Token))
        Write-Check 'attaching typed nothing into the shell' ($screen -eq $beforeAttach) `
            'the screen changed while the window attached, and nothing was typed at it'
        if ($screen -ne $beforeAttach) {
            Write-Host '--- before the window attached ---' -ForegroundColor DarkGray
            Write-Host $beforeAttach
        }
        Write-Host '--- the screen the daemon holds ---' -ForegroundColor DarkGray
        Write-Host $screen

        $state['step'] = 'relaunched'
        $state['appPid'] = $appPid
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW LOOK AT THE WINDOW. The criterion is met only if all three hold:' -ForegroundColor Yellow
        Write-Host '  - the tab is back, with the same title, and its age is the session age, not 0s;'
        Write-Host "  - the pane shows the scrollback, $($script:Token) included;"
        Write-Host '  - typing in it still works, and the prompt is empty before you type —'
        Write-Host '    a command you never typed sitting at the prompt is the re-attach defect,'
        Write-Host '    and the check above fails on it rather than leaving it for you to spot.'
        Write-Host ''
        Write-Host 'Then: -Step upgraded -NewAppBinary <path>, or -Step finish.' -ForegroundColor Yellow
    }

    'upgraded' {
        Write-Host '== step 5/6: replace the app binary; nothing should reconnect or rebind ==' -ForegroundColor Cyan
        $state = Read-State -Expected 'relaunched'
        if ($NewAppBinary -eq '') { throw 'pass -NewAppBinary <path to a different build>' }
        $NewAppBinary = (Resolve-Path $NewAppBinary).Path
        $replacement = (Get-FileHash $NewAppBinary -Algorithm SHA256).Hash
        if ($replacement -eq $state['appSha256']) {
            throw 'the replacement is byte-identical to the running binary, so it would prove nothing'
        }

        # Windows will not let a running image be replaced (os error 5), so the upgrade flow
        # *is* close-replace-relaunch. What §4 claims survives it is the daemon, not the app.
        # Not `$app`: PowerShell names are case-insensitive, so a local by that name *is* the
        # `-App` parameter, and the close below would be called on a path string.
        $running = Get-Process -Id ([int] $state['appPid']) -ErrorAction SilentlyContinue
        if ($running) { $running.CloseMainWindow() | Out-Null; Start-Sleep -Seconds 4 }
        if (Get-Process -Id ([int] $state['appPid']) -ErrorAction SilentlyContinue) {
            Stop-Process -Id ([int] $state['appPid']) -Force
            Start-Sleep -Seconds 2
        }

        Copy-Item -Force $NewAppBinary $state['appBinary']
        Write-Check 'the binary on disk changed' `
            ((Get-FileHash $state['appBinary'] -Algorithm SHA256).Hash -ne $state['appSha256']) `
            "was $($state['appSha256']), now $replacement"

        $appPid = Start-App -Binary $state['appBinary']

        $lease = Get-Content (Join-Path $RuntimeDir $script:LeaseFile) -Raw | ConvertFrom-Json
        Write-Check 'the daemon never restarted across the upgrade' `
            ($lease.pid -eq [int] $state['daemonPid'] -and $lease.launchNonce -eq $state['launchNonce']) `
            "pid $($lease.pid), launch nonce $($lease.launchNonce)"

        $rows = Get-SessionRow -Handle $state['handle']
        Write-Check 'the handle did not rotate across the upgrade' ($rows.Count -eq 1) $state['handle']
        $shellPids = Get-ShellPids -DaemonPid ([int] $state['daemonPid'])
        Write-Check 'the shell never noticed the upgrade' `
            ((@(Compare-Object $shellPids @($state['shellPids'])).Count -eq 0)) `
            "pids $($shellPids -join ', ')"

        $screen = Invoke-NysiaOk -Nysia $Nysia -Arguments @('terminal', 'read', $state['handle'], '--screen', '--no-spawn')
        Write-Check "the scrollback survived the upgrade" ($screen -match [regex]::Escape($script:Token))

        $state['step'] = 'upgraded'
        $state['appPid'] = $appPid
        $state['appSha256'] = $replacement
        Save-State $state
        Assert-NoFailures
        Write-Host ''
        Write-Host 'Then: -Step finish.' -ForegroundColor Yellow
    }

    'finish' {
        Write-Host '== step 6/6: tear down ==' -ForegroundColor Cyan
        if (-not (Test-Path $script:StatePath)) { throw "no run under $RuntimeDir" }
        $state = @{}
        (Get-Content $script:StatePath -Raw | ConvertFrom-Json).PSObject.Properties |
            ForEach-Object { $state[$_.Name] = $_.Value }

        if ($state.ContainsKey('appPid')) {
            Stop-Process -Id ([int] $state['appPid']) -Force -ErrorAction SilentlyContinue
        }
        Stop-NysiaDaemon -DaemonPid ([int] $state['daemonPid']) -RuntimeDir $RuntimeDir
        Write-Host "  torn down; the run reached step '$($state['step'])'."
    }
}
