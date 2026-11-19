param(
    [Parameter(Mandatory=$true)][string]$BaseDir,
    [Parameter(Mandatory=$true)][string]$TaskDir,
    [switch]$NoRestart,
    [int]$WaitForPid = 0
)
$ErrorActionPreference = 'Stop'
$base = [IO.Path]::GetFullPath($BaseDir)
$task = [IO.Path]::GetFullPath($TaskDir)
$package = Join-Path $task 'package'
$backupRoot = Join-Path $task 'backup'
$pendingRoot = Join-Path $task 'pending'
# Deliberately exclude library.db, config.json, local title database,
# keys, screenshots, icons, torrent state and downloads.
$files = @('nsl.exe', 'nsl-manifest.json')
$applied = @()
try {
    # Reverify staged content immediately before applying it. The package
    # could have changed after the download-and-confirm stage.
    $sourceExe = Join-Path $package 'nsl.exe'
    $sourceManifest = Join-Path $package 'nsl-manifest.json'
    if (-not (Test-Path -LiteralPath $sourceExe -PathType Leaf) -or
        -not (Test-Path -LiteralPath $sourceManifest -PathType Leaf)) {
        throw 'The staged NSL update is missing its executable or manifest.'
    }
    $manifest = Get-Content -LiteralPath $sourceManifest -Raw | ConvertFrom-Json
    $expectedHash = [string]$manifest.sha256.'nsl.exe'
    if ($manifest.application -cne 'NSL' -or
        $manifest.platform -cne 'windows-x86_64' -or
        $manifest.commit -notmatch '^[0-9a-fA-F]{40}$' -or
        $expectedHash -notmatch '^[0-9a-fA-F]{64}$') {
        throw 'The staged NSL update manifest is invalid.'
    }
    $sourceHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
    if ($sourceHash -cne $expectedHash.ToUpperInvariant()) {
        throw 'The staged NSL executable does not match its SHA-256 manifest.'
    }
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
    # Wait for the exact process that requested this update. A fixed sleep
    # could expire before the GUI has released its executable on slow PCs.
    if ($WaitForPid -gt 0) {
        $oldProcess = Get-Process -Id $WaitForPid -ErrorAction SilentlyContinue
        if ($null -ne $oldProcess) {
            try {
                if (-not $oldProcess.WaitForExit(60000)) {
                    throw "NSL (PID $WaitForPid) did not exit; no files were changed."
                }
            } finally {
                $oldProcess.Dispose()
            }
        }
    } else {
        Start-Sleep -Milliseconds 500
    }
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
    $installedHash = (Get-FileHash -LiteralPath (Join-Path $base 'nsl.exe') -Algorithm SHA256).Hash
    if ($installedHash -cne $sourceHash) {
        throw 'Installed NSL executable SHA-256 differs from the verified update.'
    }
} catch {
    $failure = $_
    $rollbackErrors = @()
    [array]::Reverse($applied)
    foreach ($relative in $applied) {
        $target = Join-Path $base $relative
        $backup = Join-Path $backupRoot $relative
        try {
            if ([IO.File]::Exists($backup)) {
                if ([IO.File]::Exists($target)) {
                    # .NET Framework's File.Replace needs a non-null backup.
                    $failedVersion = Join-Path (Join-Path $task 'failed') $relative
                    New-Item -ItemType Directory -Path (Split-Path -Parent $failedVersion) -Force | Out-Null
                    [IO.File]::Replace($backup, $target, $failedVersion, $true)
                } else {
                    [IO.File]::Move($backup, $target)
                }
            } elseif ([IO.File]::Exists($target)) {
                [IO.File]::Delete($target)
            }
        } catch {
            $rollbackErrors += "${relative}: $_"
        }
    }
    $reason = "NSL update failed: $failure"
    if ($rollbackErrors.Count -gt 0) {
        $reason += "`nRollback errors: $($rollbackErrors -join '; ')"
    }
    # This process is hidden; keep a readable error for troubleshooting.
    try {
        [IO.File]::WriteAllText(
            (Join-Path $task 'update-error.log'),
            "$(Get-Date -Format o)`n$reason`n",
            [Text.UTF8Encoding]::new($false)
        )
    } catch {}
    if (-not $NoRestart -and $rollbackErrors.Count -eq 0 -and
        (Test-Path -LiteralPath (Join-Path $base 'nsl.exe') -PathType Leaf)) {
        # If the original app is still running after a timeout, do not start
        # a second instance. Otherwise reopen the restored version so a
        # failed update does not leave the user without the application.
        $stillRunning = $WaitForPid -gt 0 -and
            $null -ne (Get-Process -Id $WaitForPid -ErrorAction SilentlyContinue)
        if (-not $stillRunning) {
            try {
                Start-Process -FilePath (Join-Path $base 'nsl.exe') -WorkingDirectory $base
            } catch {}
        }
    }
    Write-Error $reason
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
