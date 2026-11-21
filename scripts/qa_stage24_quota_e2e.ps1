param(
    [string]$Log = 'docs\qa\QA_NSL_STAGE24_REAL_QUOTA_E2E_LOCAL_2026-11-21.txt'
)
$ErrorActionPreference = 'Stop'

# Windows-only, deliberately isolated WSL tmpfs. Do not touch existing host
# volumes, other WSL mounts, game files, emulator instances, or user profiles.
$name = 'nsl-stage24-quota-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
$linux = '/tmp/' + $name
$windows = '\\wsl.localhost\Ubuntu\tmp\' + $name
$mounted = $false
$created = $false
$previousWindows = [Environment]::GetEnvironmentVariable('NSL_STAGE24_QUOTA_UNC', 'Process')
$previousLinux = [Environment]::GetEnvironmentVariable('NSL_STAGE24_QUOTA_LINUX_ROOT', 'Process')
$result = 1
try {
    if (Test-Path -LiteralPath $windows) {
        throw 'Refusing pre-existing Stage 24 quota mount'
    }
    & wsl.exe -d Ubuntu -u root -- mkdir $linux
    if ($LASTEXITCODE -ne 0) { throw 'Could not create isolated WSL test directory' }
    $created = $true
    & wsl.exe -d Ubuntu -u root -- mount -t tmpfs -o 'size=4m,nosuid,nodev' tmpfs $linux
    if ($LASTEXITCODE -ne 0) { throw 'Could not mount isolated 4 MiB WSL tmpfs' }
    $mounted = $true
    & wsl.exe -d Ubuntu -u root -- chmod 777 $linux
    if ($LASTEXITCODE -ne 0) { throw 'Could not access isolated mount from Windows' }
    if (-not (Test-Path -LiteralPath $windows)) {
        throw 'Windows UNC cannot access private WSL quota directory'
    }

    [Environment]::SetEnvironmentVariable('NSL_STAGE24_QUOTA_UNC', $windows, 'Process')
    [Environment]::SetEnvironmentVariable('NSL_STAGE24_QUOTA_LINUX_ROOT', $linux, 'Process')
    # PowerShell 5.1 wraps native Cargo stderr as an error record even for
    # successful builds. Use Cargo's actual exit code, not this wrapper text.
    $ErrorActionPreference = 'Continue'
    & cargo test --locked stage24_real_quota_exhaustion_download_retry_exact_bytes -- --ignored --nocapture --test-threads=1 *> $Log
    $result = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Get-Content -LiteralPath $Log -Tail 48
    if ($result -eq 0) {
        $proof = Select-String -LiteralPath $Log -SimpleMatch 'STAGE24_QUOTA_EXHAUSTION_RECOVERED=PASS' -Quiet
        if (-not $proof) { $result = 1; Write-Output 'STAGE24_MISSING_QUOTA_RECOVERY_PROOF' }
    }
} finally {
    [Environment]::SetEnvironmentVariable('NSL_STAGE24_QUOTA_UNC', $previousWindows, 'Process')
    [Environment]::SetEnvironmentVariable('NSL_STAGE24_QUOTA_LINUX_ROOT', $previousLinux, 'Process')
    if ($mounted) {
        & wsl.exe -d Ubuntu -u root -- umount $linux
        if ($LASTEXITCODE -ne 0) {
            $result = 1
            Write-Output "STAGE24_PRIVATE_MOUNT_CLEANUP_FAILED=$linux"
        } else {
            $mounted = $false
        }
    }
    if ($created -and -not $mounted) {
        & wsl.exe -d Ubuntu -u root -- rmdir $linux
        if ($LASTEXITCODE -ne 0) {
            $result = 1
            Write-Output "STAGE24_PRIVATE_DIRECTORY_CLEANUP_FAILED=$linux"
        }
    }
    if (-not $mounted) {
        Write-Output 'STAGE24_ISOLATED_QUOTA_UNMOUNTED=YES'
    }
}
exit $result
