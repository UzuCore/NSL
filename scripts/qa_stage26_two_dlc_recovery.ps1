# Isolated, opt-in Stage 26 read-only real-DLC test. Only the Rust test writes
# temporary files, and only into the caller-provided QA root. Never alter
# the user's real game library, prod.keys, saves, or emulator processes.
param(
    [string]$SourceRoot = 'E:\NSW',
    [string]$QaRoot = 'E:\NSL_QA_20261120',
    [string]$BaseId = '0100800015926000'
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $SourceRoot).ProviderPath.TrimEnd('\')
$qa = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
$prefix = $source + '\'
if ($qa.Equals($source, [StringComparison]::OrdinalIgnoreCase) -or
    $qa.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Stage26 QA scratch directory must be outside the original game library.'
}
if ($BaseId -notmatch '^[0-9A-Fa-f]{16}$') { throw 'Invalid 16-digit base Title ID.' }
$items = @(Get-ChildItem -LiteralPath $source -File -Filter "*$BaseId*.nsz")
if ($items.Count -ne 1) { throw "Expected exactly one read-only base NSZ: $BaseId" }
$dlcDir = Join-Path $source '_dlc'
if (-not (Test-Path -LiteralPath $dlcDir -PathType Container)) {
    throw 'Read-only DLC source folder does not exist.'
}
foreach ($dlc in @('010080001592701D', '010080001592701C')) {
    $matches = @(Get-ChildItem -LiteralPath $dlcDir -File -Filter "*$dlc*.nsp")
    if ($matches.Count -ne 1) { throw "Expected exactly one read-only DLC NSP: $dlc" }
}
$disk = (Get-Item -LiteralPath $qa).PSDrive
if ($null -eq $disk -or $disk.Free -lt 16GB) {
    throw 'Stage26 requires at least 16 GiB available on the isolated QA drive.'
}
$previous = @{}
foreach ($key in @('NSL_STAGE13_SOURCE_ROOT', 'NSL_STAGE13_BASE', 'NSL_STAGE13_DLC_DIR', 'NSL_STAGE13_QA_ROOT')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
$project = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
try {
    $env:NSL_STAGE13_SOURCE_ROOT = $source
    $env:NSL_STAGE13_BASE = $items[0].FullName
    $env:NSL_STAGE13_DLC_DIR = $dlcDir
    $env:NSL_STAGE13_QA_ROOT = $qa
    Push-Location -LiteralPath $project
    try {
        & cargo test --locked stage26_real_two_dlc_batch_cancel_restart_and_cleanup -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw "Stage26 real two-DLC QA failed: exit=$LASTEXITCODE" }
        Write-Output 'STAGE26_REAL_TWO_DLC_RECOVERY_SCRIPT=PASS'
    } finally {
        Pop-Location
    }
} finally {
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
}
