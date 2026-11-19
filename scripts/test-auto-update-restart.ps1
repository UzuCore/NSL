param([int]$ObserveSeconds = 3)

# Real end-to-end update -> replace -> restart smoke test. Only an isolated,
# randomly named temporary installation is modified. Never touches an
# existing NSL executable or sends messages to other running NSL instances.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$installer = Join-Path $root 'scripts\apply-update.ps1'
$sourceExe = Join-Path $root 'dist\nsl.exe'
$sourceManifest = Join-Path $root 'dist\nsl-manifest.json'
$latest = Get-Content -LiteralPath $sourceManifest -Raw | ConvertFrom-Json
$expectedHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
$sandbox = Join-Path ([IO.Path]::GetTempPath()) ('NSL_restart_qa_' + [guid]::NewGuid().ToString('N'))
$bin = Join-Path $sandbox 'target\release'
$task = Join-Path $sandbox 'temp\nsl_update_restart_qa'
$package = Join-Path $task 'package'
$startedPid = 0

function Sha256([string]$path) {
    return (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
}

try {
    New-Item -ItemType Directory -Path $bin,$package,(Join-Path $sandbox 'titledb') -Force | Out-Null
    # A minimal valid project layout causes a target/release GUI to use its
    # OWN isolated data directory; it never scans real files or copies keys.
    [IO.File]::WriteAllText(
        (Join-Path $sandbox 'config.json'),
        '{"scan_folders":[],"settings":{"auto_update_check":"0","last_tab_key":"LIBRARY"}}',
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllBytes((Join-Path $sandbox 'library.db'),[byte[]]@())
    [IO.File]::WriteAllText((Join-Path $sandbox 'prod.keys'), 'unrelated user sentinel', [Text.UTF8Encoding]::new($false))
    $protected = @{}
    foreach ($name in @('config.json','library.db','prod.keys')) {
        $protected[$name] = Sha256 (Join-Path $sandbox $name)
    }
    $oldExe = Join-Path $bin 'nsl.exe'
    [IO.File]::WriteAllText($oldExe, 'old executable in independent sandbox', [Text.UTF8Encoding]::new($false))
    $oldHash = Sha256 $oldExe
    [IO.File]::WriteAllText((Join-Path $bin 'nsl-manifest.json'), '{"commit":"old"}', [Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath $sourceExe -Destination (Join-Path $package 'nsl.exe')
    Copy-Item -LiteralPath $sourceManifest -Destination (Join-Path $package 'nsl-manifest.json')
    $installerStdout = Join-Path $task 'installer-stdout.log'
    $installerStderr = Join-Path $task 'installer-stderr.log'
    $args = @(
        '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass',
        '-File',('"' + $installer + '"'),
        '-BaseDir',('"' + $bin + '"'),
        '-TaskDir',('"' + $task + '"'),
        '-WaitForPid','0'
    )
    # Start-Process -Wait waits for an entire descendant process tree on
    # Windows. The installer starts NSL, so -Wait would block on the very GUI
    # we are trying to inspect. .NET WaitForExit waits only for powershell.exe.
    $psi = [Diagnostics.ProcessStartInfo]::new('powershell.exe')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $psi.Arguments = $args -join ' '
    $installerProcess = [Diagnostics.Process]::Start($psi)
    if (-not $installerProcess.WaitForExit(30000)) {
        $installerProcess.Kill()
        throw 'Isolated update installer did not finish within 30 seconds'
    }
    $stdoutText = $installerProcess.StandardOutput.ReadToEnd()
    $stderrText = $installerProcess.StandardError.ReadToEnd()
    [IO.File]::WriteAllText($installerStdout, $stdoutText, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($installerStderr, $stderrText, [Text.UTF8Encoding]::new($false))
    if ($installerProcess.ExitCode -ne 0) {
        throw "Isolated update installer failed: $stderrText"
    }
    $installerProcess.Dispose()
    if ((Sha256 $oldExe) -cne $expectedHash -or
        (Sha256 (Join-Path $task 'backup\nsl.exe')) -cne $oldHash) {
        throw 'Isolated update did not install the official executable and back up the old one'
    }
    $installedManifest = Get-Content (Join-Path $bin 'nsl-manifest.json') -Raw | ConvertFrom-Json
    if ($installedManifest.commit -cne $latest.commit -or
        (Get-Content (Join-Path $task 'apply-ready.flag') -Raw) -cne $latest.commit) {
        throw 'Update manifest and parent/child handshake do not match official release'
    }
    Write-Output 'PASS: isolated update installed official executable and verified original backup'

    $success = Join-Path $task 'apply-success.log'
    if (-not (Test-Path -LiteralPath $success)) { throw 'Installer did not record the restarted child PID' }
    $status = Get-Content -LiteralPath $success -Raw
    if ($status -notmatch 'Restart PID: (\d+)') { throw "Missing restart PID: $status" }
    $startedPid = [int]$Matches[1]
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $running = $null
    while ($clock.Elapsed.TotalSeconds -lt 15) {
        $running = Get-Process -Id $startedPid -ErrorAction SilentlyContinue
        if ($null -ne $running) {
            $running.Refresh()
            if ($running.HasExited) { throw "Isolated restarted NSL exited early: $startedPid" }
            if ($running.MainWindowHandle -ne [IntPtr]::Zero) { break }
        }
        Start-Sleep -Milliseconds 200
    }
    if ($null -eq $running -or $running.MainWindowHandle -eq [IntPtr]::Zero) {
        throw "Isolated restarted NSL did not display its GUI (PID $startedPid)"
    }
    if (-not $running.Path.StartsWith($bin,[StringComparison]::OrdinalIgnoreCase)) {
        throw "Unexpected restarted executable path: $($running.Path)"
    }
    for ($i = 0; $i -lt $ObserveSeconds * 4; $i++) {
        Start-Sleep -Milliseconds 250
        $running.Refresh()
        if ($running.HasExited) { throw 'Restarted NSL exited during observation' }
    }
    if ($running.MainWindowTitle -notlike '*NSL*') {
        throw "Unexpected restart window title: $($running.MainWindowTitle)"
    }
    Write-Output ("PASS: isolated update restarted NSL GUI, pid={0}, title={1}" -f $startedPid,$running.MainWindowTitle)
    # SQLite legitimately changes when NSL initializes the new sandbox.
    # User-provided configuration and keys must be kept byte-for-byte.
    foreach ($name in @('config.json','prod.keys')) {
        if ((Sha256 (Join-Path $sandbox $name)) -cne $protected[$name]) {
            throw "Installer unexpectedly modified unrelated user state: $name"
        }
    }
    Write-Output 'PASS: isolated user configuration and keys preserved'
    Write-Output 'AUTO_UPDATE_RESTART_GUI_QA=PASS'
} finally {
    if ($startedPid -gt 0) {
        $p = Get-Process -Id $startedPid -ErrorAction SilentlyContinue
        if ($null -ne $p) {
            try {
                if (-not $p.HasExited -and
                    $p.Path.StartsWith($bin,[StringComparison]::OrdinalIgnoreCase)) {
                    $null = $p.CloseMainWindow()
                    if (-not $p.WaitForExit(3000)) {
                        # This is only the short-lived GUI created by THIS QA.
                        Stop-Process -Id $startedPid -Force -ErrorAction SilentlyContinue
                    }
                }
            } finally { $p.Dispose() }
        }
    }
    if (Test-Path -LiteralPath $sandbox) {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
}
