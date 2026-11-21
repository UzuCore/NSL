# Stage29: isolated synthetic 64MiB hardlink-denied, mid-copy OS kill.
# Only a test-owned subprocess gets terminated, no real game libraries.
param([string]$QaRoot = 'E:\NSL_QA_20261120')
$ErrorActionPreference = 'Stop'
$qa = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
$source = (Resolve-Path -LiteralPath 'E:\NSW').ProviderPath.TrimEnd('\')
if ($qa.Equals($source,[StringComparison]::OrdinalIgnoreCase) -or
    $qa.StartsWith(($source + '\'),[StringComparison]::OrdinalIgnoreCase) -or
    $source.StartsWith(($qa + '\'),[StringComparison]::OrdinalIgnoreCase)) {
    throw 'Stage29 scratch and source game library must be disjoint.'
}
if ((Get-Item -LiteralPath $qa).PSDrive.Free -lt 2GB) {
    throw 'Stage29 requires 2GiB free space for private synthetic data.'
}
$old = [Environment]::GetEnvironmentVariable('NSL_STAGE29_QA_ROOT','Process')
try {
    $env:NSL_STAGE29_QA_ROOT = $qa
    Push-Location -LiteralPath ((Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath)
    try {
        & cargo test --locked stage29_real_fallback_midcopy_kill_and_retry_preserves_original -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) {
            throw "Stage29 owned fallback midcopy kill/retry failed: exit=$LASTEXITCODE"
        }
        Write-Output 'STAGE29_OWNED_FALLBACK_MIDCOPY_SCRIPT=PASS'
    } finally { Pop-Location }
} finally {
    [Environment]::SetEnvironmentVariable('NSL_STAGE29_QA_ROOT',$old,'Process')
}
