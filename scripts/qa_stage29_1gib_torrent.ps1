# Stage29: opt-in 1GiB local-only BitTorrent selected-file E2E.
# Test uses two synthetic 512MiB files and a 32MiB unselected sentinel.
# Real game library, keys, external peers, other emulator instances untouched.
param([string]$QaRoot = 'E:\NSL_QA_20261120')
$ErrorActionPreference = 'Stop'
$qa = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
$game = (Resolve-Path -LiteralPath 'E:\NSW').ProviderPath.TrimEnd('\')
if (-not $qa.EndsWith('\NSL_QA_20261120',[StringComparison]::OrdinalIgnoreCase) -or
    $qa.Equals($game,[StringComparison]::OrdinalIgnoreCase) -or
    $qa.StartsWith(($game+'\'),[StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refuse any non-owned or real-library destination for 1GiB test.'
}
if ((Get-Item -LiteralPath $qa).PSDrive.Free -lt 8GB) {
    throw 'At least 8 GiB of free QA space required.'
}
$previous = [Environment]::GetEnvironmentVariable('NSL_STAGE29_TORRENT_QA_ROOT','Process')
try {
    $env:NSL_STAGE29_TORRENT_QA_ROOT = $qa
    Push-Location -LiteralPath ((Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath)
    try {
        & cargo test --locked stage29_1gib_two_selected_files_pause_restart_exact_sha256 -- --ignored --nocapture --test-threads=1
        if($LASTEXITCODE -ne 0) {
            throw "Stage29 1GiB selected-only loopback torrent test failed: $LASTEXITCODE"
        }
        Write-Output 'STAGE29_1GIB_TORRENT_SCRIPT=PASS'
    } finally { Pop-Location }
} finally {
    [Environment]::SetEnvironmentVariable('NSL_STAGE29_TORRENT_QA_ROOT',$previous,'Process')
}
