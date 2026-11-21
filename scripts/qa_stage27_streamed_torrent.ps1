# Opt-in 256MiB loopback-only BitTorrent streaming regression.
# The Rust test owns a unique temporary folder in the QA root; no keys,
# games, production torrent sessions, or outside seeders are contacted.
param([string]$QaRoot = 'E:\NSL_QA_20261120')
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
$source = (Resolve-Path -LiteralPath 'E:\NSW').ProviderPath.TrimEnd('\')
if ($root.Equals($source, [StringComparison]::OrdinalIgnoreCase) -or
    $root.StartsWith(($source + '\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Do not run Stage27 streaming QA inside the real game library.'
}
if ((Get-Item -LiteralPath $root).PSDrive.Free -lt 5GB) {
    throw 'At least 5 GiB isolated scratch free space required.'
}
$old = [Environment]::GetEnvironmentVariable('NSL_STAGE27_TORRENT_QA_ROOT','Process')
$project = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
try {
    $env:NSL_STAGE27_TORRENT_QA_ROOT = $root
    Push-Location -LiteralPath $project
    try {
        & cargo test --locked stage27_streamed_256mib_local_torrent_restart_sha256 -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) {
            throw "Stage27 streamed torrent test failed: exit=$LASTEXITCODE"
        }
        Write-Output 'STAGE27_256MIB_TORRENT_STREAMING_SCRIPT=PASS'
    } finally { Pop-Location }
} finally {
    [Environment]::SetEnvironmentVariable('NSL_STAGE27_TORRENT_QA_ROOT',$old,'Process')
}
