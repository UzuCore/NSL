param(
    [Parameter(Mandatory=$true)][string]$BaseDir,
    [Parameter(Mandatory=$true)][string]$TaskDir,
    [switch]$NoRestart
)
$ErrorActionPreference = 'Stop'
$base = [IO.Path]::GetFullPath($BaseDir)
$task = [IO.Path]::GetFullPath($TaskDir)
$package = Join-Path $task 'package'
$backupRoot = Join-Path $task 'backup'
$pendingRoot = Join-Path $task 'pending'
# Deliberately exclude library.db, config.json, local title database,
# keys, screenshots, icons, torrent state and downloads.
$files = @('nsl.exe', 'bin/nstool.exe', 'bin/hactool.exe', 'nsl-rust-manifest.json')
$applied = @()
try {
    foreach ($relative in $files) {
        $source = Join-Path $package $relative
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Update file missing: $relative"
        }
        $pending = Join-Path $pendingRoot $relative
        $parent = Split-Path -Parent $pending
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
        Copy-Item -LiteralPath $source -Destination $pending -Force
    }
    # The application must have exited before Windows can replace its EXE.
    Start-Sleep -Seconds 2
    foreach ($relative in $files) {
        $target = Join-Path $base $relative
        $pending = Join-Path $pendingRoot $relative
        $backup = Join-Path $backupRoot $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
        New-Item -ItemType Directory -Path (Split-Path -Parent $backup) -Force | Out-Null
        $replaced = $false
        for ($attempt = 0; $attempt -lt 30 -and -not $replaced; $attempt++) {
            try {
                if ([IO.File]::Exists($target)) {
                    [IO.File]::Replace($pending, $target, $backup, $true)
                } else {
                    [IO.File]::Move($pending, $target)
                }
                $replaced = $true
                $applied += $relative
            } catch {
                if ($attempt -eq 29) { throw }
                Start-Sleep -Milliseconds 500
            }
        }
    }
} catch {
    $failure = $_
    [array]::Reverse($applied)
    foreach ($relative in $applied) {
        $target = Join-Path $base $relative
        $backup = Join-Path $backupRoot $relative
        if ([IO.File]::Exists($backup)) {
            if ([IO.File]::Exists($target)) {
                # .NET Framework's File.Replace requires a non-null backup
                # path on Windows, including during rollback.
                $failedVersion = Join-Path (Join-Path $task 'failed') $relative
                New-Item -ItemType Directory -Path (Split-Path -Parent $failedVersion) -Force | Out-Null
                [IO.File]::Replace($backup, $target, $failedVersion, $true)
            } else {
                [IO.File]::Move($backup, $target)
            }
        } elseif ([IO.File]::Exists($target)) {
            [IO.File]::Delete($target)
        }
    }
    Write-Error "NSL update failed and attempted rollback: $failure"
    exit 1
} finally {
    if (Test-Path -LiteralPath $pendingRoot) {
        Remove-Item -LiteralPath $pendingRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
if (-not $NoRestart) {
    Start-Process -FilePath (Join-Path $base 'nsl.exe') -WorkingDirectory $base
    # An incomplete update remains on disk for diagnosis; a successful one
    # only contains the previous binaries and may be removed by the user.
}
