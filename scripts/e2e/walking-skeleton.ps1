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

    **Step 4 types, and then proves that typing could have caught something.** Comparing the
    screen either side of an attach only catches phantom input that *echoes*, and the reported
    defect did not echo: F3 put a recalled line into the editor without submitting it, and the
    user's next command ran concatenated onto it. So after the comparison the step sends a
    command of its own and asserts a whole screen line equal to a token the shell computed —
    which a concatenated line cannot produce — and then repeats it with two characters left
    sitting in the editor and asserts the same check fails. A check with no demonstration that
    it can fail is the one this replaces (#44).

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

# The second token: computed by the shell *after* the relaunch, which is the half of the
# re-attach defect a screen comparison cannot see.
#
# Neither token contains the other, deliberately. The reported defect was `echo STILL-ALIVE`
# running as `echo NYSIA-%NYS%echo STILL-ALIVE` — a line whose *output* still contains the
# second token as a substring. A check that searched for it would have passed on the very
# corruption it was written for. What is asserted instead is a whole screen line equal to the
# token, which the concatenation cannot produce.
$script:Reattach = 'NYSIA-REATTACH-84'
$script:ReattachRecipe = @{
    pwsh     = @('Write-Output ("NYSIA" + "-REATTACH-" + (2*42))')
    cmd      = @('set /a RE=2*42', 'echo NYSIA-REATTACH-%RE%')
    git_bash = @('echo "NYSIA-REATTACH-$((2*42))"')
}

# The same shape again, over a variable nothing has set, for the control that proves the check
# above can fail. A second recipe rather than a re-run of the first: after the real check `RE`
# holds 84, so a control that reused it would print the token even with its first line broken
# and would report a working check as working for the wrong reason.
$script:Control = 'NYSIA-CONTROL-21'
$script:ControlRecipe = @{
    pwsh     = @('Write-Output ("NYSIA" + "-CONTROL-" + (3*7))')
    cmd      = @('set /a CO=3*7', 'echo NYSIA-CONTROL-%CO%')
    git_bash = @('echo "NYSIA-CONTROL-$((3*7))"')
}

# What the control leaves sitting in the line editor: two characters nobody submitted. The
# defect leaves a whole recalled command line there, but the property under test is "something
# was waiting", and two characters are the smallest version of it this can type.
$script:Prepended = 'X_'

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

# The rendered screen the daemon holds: what a person looking at the pane would see.
function Read-Screen {
    param([Parameter(Mandatory)] [string] $Handle)

    return Invoke-NysiaOk -Nysia $Nysia -Arguments @(
        'terminal', 'read', $Handle, '--screen', '--no-spawn'
    )
}

# The scrolled-out lines as well as the viewport, for a claim about something that may have
# scrolled off the top by the time it is checked.
function Read-Scrollback {
    param([Parameter(Mandatory)] [string] $Handle)

    return Invoke-NysiaOk -Nysia $Nysia -Arguments @(
        'terminal', 'read', $Handle, '--stream', '--no-spawn'
    )
}

# Let the session settle. `idle` is a heuristic by construction — a repainting TUI never truly
# stops — but a shell that has finished a command does, and every wait here is for one of those.
function Wait-Idle {
    param([Parameter(Mandatory)] [string] $Handle, [int] $TimeoutMs = 20000)

    $null = Invoke-Nysia -Nysia $Nysia -Arguments @(
        'terminal', 'wait', $Handle, '--for', 'idle', '--timeout-ms', "$TimeoutMs", '--no-spawn'
    )
}

# Type at the session the way the daemon's own clients do, and wait for what it produced.
function Send-Line {
    param(
        [Parameter(Mandatory)] [string] $Handle,
        [Parameter(Mandatory)] [AllowEmptyString()] [string] $Text,
        [switch] $NoEnter
    )

    $arguments = @('terminal', 'send', $Handle, '--text', $Text)
    if (-not $NoEnter) { $arguments += '--enter' }
    $arguments += '--no-spawn'
    $null = Invoke-NysiaOk -Nysia $Nysia -Arguments $arguments
    Wait-Idle -Handle $Handle
}

function Send-Recipe {
    param([Parameter(Mandatory)] [string] $Handle, [Parameter(Mandatory)] [string[]] $Lines)

    foreach ($line in $Lines) { Send-Line -Handle $Handle -Text $line }
}

# Ctrl-C, to leave a clean prompt behind for whatever runs next.
function Clear-Prompt {
    param([Parameter(Mandatory)] [string] $Handle)

    $null = Invoke-NysiaOk -Nysia $Nysia -Arguments @(
        'terminal', 'send', $Handle, '--interrupt', '--no-spawn'
    )
    Wait-Idle -Handle $Handle
}

# **A whole line, never a substring.** See `$script:Reattach` for why the difference is the
# whole point: the corruption this looks for produces output that *contains* the token.
function Test-HasLine {
    param([Parameter(Mandatory)] [AllowEmptyString()] [string] $Text, [Parameter(Mandatory)] [string] $Line)

    foreach ($row in ($Text -split "`r?`n")) {
        if ($row.Trim() -eq $Line) { return $true }
    }
    return $false
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
        $lease = Get-Content (Get-NysiaLeasePath -RuntimeDir $RuntimeDir) -Raw | ConvertFrom-Json
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

        $screen = Read-Screen -Handle $handle
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

        $screen = Read-Screen -Handle $state['handle']
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

        $handle = $state['handle']
        $shell = $state['shell']

        # The screen as it stands with no window attached to it. Everything between here and
        # the comparison below is the app attaching, and the app attaching must not type
        # anything: a replay carries every terminal query the child ever wrote, a renderer
        # answers a query when it parses one and cannot tell a replayed one from a live one,
        # and ConPTY reads the answer to a cursor-position report as F3 — which `cmd` treats as
        # recall-previous-command. This is the defect reported against v0.1, made into a check
        # rather than an instruction to look at the screen and notice.
        $beforeAttach = Read-Screen -Handle $handle

        $appPid = Start-App -Binary $state['appBinary']

        $daemonPid = [int] $state['daemonPid']
        $lease = Get-Content (Get-NysiaLeasePath -RuntimeDir $RuntimeDir) -Raw | ConvertFrom-Json
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
        Wait-Idle -Handle $handle

        $screen = Read-Screen -Handle $handle
        Write-Check "the scrollback still shows $($script:Token)" ($screen -match [regex]::Escape($script:Token))
        Write-Check 'attaching typed nothing the shell echoed' ($screen -eq $beforeAttach) `
            'the screen changed while the window attached, and nothing was typed at it'
        if ($screen -ne $beforeAttach) {
            Write-Host '--- before the window attached ---' -ForegroundColor DarkGray
            Write-Host $beforeAttach
        }
        Write-Host '--- the screen the daemon holds ---' -ForegroundColor DarkGray
        Write-Host $screen

        # **The check the screen comparison above is a proxy for**, and the reason it is not
        # enough on its own (#44). That comparison catches phantom input only if it *echoes*,
        # and it says nothing about what the child would do with the next thing typed at it.
        # The reported defect was not an echo: F3 put a recalled line into the editor without
        # submitting it, and the user's next command ran concatenated onto it — `echo
        # STILL-ALIVE` ran as `echo NYSIA-%NYS%echo STILL-ALIVE`.
        #
        # So this types, and asserts the child ran exactly what was typed and nothing else: a
        # whole screen line equal to a token the shell computed. The concatenation cannot
        # produce one, because whatever was already in the editor is part of the line the shell
        # parses, and either the command fails or its output carries the leftovers with it.
        #
        # What this deliberately does **not** do is assert that the replayed scrollback
        # contained a query. There is no way to: the replay ring is raw bytes and
        # `terminal read` hands back text with the escapes already stripped, so nothing a
        # script can call can see one. The answer is not to assert a precondition it cannot
        # reach but to stop depending on it — the claim below is about what the child ran, and
        # the control after it proves the claim is one that can fail.
        Write-Host ''
        Write-Host "--- typing $($script:Reattach) at the re-attached session ---" -ForegroundColor DarkGray
        Send-Recipe -Handle $handle -Lines $script:ReattachRecipe[$shell]
        $typed = Read-Screen -Handle $handle
        Write-Check "the shell ran exactly what was sent, and computed $($script:Reattach)" `
            (Test-HasLine -Text $typed -Line $script:Reattach) `
            'no line is that token alone; something was already in the line editor'

        # Traps register #12: a gate ships with a proof that it trips. Two characters are put
        # into the editor without a newline — which is the shape of the defect, a line nobody
        # submitted — and the same recipe is sent after them. It must not produce its token.
        # Without this the check above is a sentence about a property nothing has demonstrated
        # it can observe, which is exactly the complaint #44 makes about the step it replaces.
        Send-Line -Handle $handle -Text $script:Prepended -NoEnter
        Send-Recipe -Handle $handle -Lines $script:ControlRecipe[$shell]
        $control = Read-Screen -Handle $handle
        Write-Check "that check fails when '$($script:Prepended)' is waiting at the prompt" `
            (-not (Test-HasLine -Text $control -Line $script:Control)) `
            "$($script:Control) appeared even with something prepended, so the check above proves nothing"
        Clear-Prompt -Handle $handle
        Write-Host '--- after the assertion and its control ---' -ForegroundColor DarkGray
        Write-Host (Read-Screen -Handle $handle)

        $state['step'] = 'relaunched'
        $state['appPid'] = $appPid
        Save-State $state
        Assert-NoFailures

        Write-Host ''
        Write-Host 'NOW LOOK AT THE WINDOW. The criterion is met only if all three hold:' -ForegroundColor Yellow
        Write-Host '  - the tab is back, with the same title, and its age is the session age, not 0s;'
        Write-Host "  - the pane shows the scrollback, $($script:Token) included;"
        Write-Host "  - the pane also shows $($script:Reattach), then the failed control below it,"
        Write-Host '    and typing in it still works. The script typed those two itself through the'
        Write-Host '    daemon; what it cannot do is type at the *window*, so try a line of your own.'
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

        $lease = Get-Content (Get-NysiaLeasePath -RuntimeDir $RuntimeDir) -Raw | ConvertFrom-Json
        Write-Check 'the daemon never restarted across the upgrade' `
            ($lease.pid -eq [int] $state['daemonPid'] -and $lease.launchNonce -eq $state['launchNonce']) `
            "pid $($lease.pid), launch nonce $($lease.launchNonce)"

        $rows = Get-SessionRow -Handle $state['handle']
        Write-Check 'the handle did not rotate across the upgrade' ($rows.Count -eq 1) $state['handle']
        $shellPids = Get-ShellPids -DaemonPid ([int] $state['daemonPid'])
        Write-Check 'the shell never noticed the upgrade' `
            ((@(Compare-Object $shellPids @($state['shellPids'])).Count -eq 0)) `
            "pids $($shellPids -join ', ')"

        # The scrollback, not the screen. Step 4 types at the session twice after its own
        # checks — the assertion it makes and the control that proves the assertion can fail —
        # so the token this looks for may have scrolled off the top of a short pane by now.
        # Read against the viewport alone, a pass would mean "the pane is short" as readily as
        # "the scrollback survived".
        $scrollback = Read-Scrollback -Handle $state['handle']
        Write-Check "the scrollback survived the upgrade" ($scrollback -match [regex]::Escape($script:Token))

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
