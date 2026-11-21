# Stage 27 opt-in destructive-in-child-only crash test.
# Protect real game/DLC source files and kill only this script's test-owned
# child. The Rust parent owns all scratch files and automatically cleans them.
param(
    [string]$SourceRoot = 'E:\NSW',
    [string]$QaRoot = 'E:\NSL_QA_20261120',
    [string]$BaseId = '0100800015926000',
    [string]$DlcId = '010080001592701D'
)
$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $SourceRoot).ProviderPath.TrimEnd('\')
$qa = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
if ($qa.Equals($source, [StringComparison]::OrdinalIgnoreCase) -or
    $qa.StartsWith(($source + '\'), [StringComparison]::OrdinalIgnoreCase) -or
    $source.StartsWith(($qa + '\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'QA and original game library must be disjoint.'
}
if ($BaseId -ne '0100800015926000' -or $DlcId -ne '010080001592701D') {
    throw 'Only the known Stage27 read-only real-content QA fixture is authorized.'
}
$base = @(Get-ChildItem -LiteralPath $source -File -Filter "*$BaseId*.nsz")
$dlcRoot = Join-Path $source '_dlc'
$patch = @(Get-ChildItem -LiteralPath $dlcRoot -File -Filter "*$DlcId*.nsp")
if ($base.Count -ne 1 -or $patch.Count -ne 1) {
    throw 'The unique authorized game and DLC package fixtures are missing.'
}
if ((Get-Item -LiteralPath $qa).PSDrive.Free -lt 16GB) {
    throw 'Stage27 requires at least 16 GiB of free isolated QA scratch space.'
}
$vars = @('NSL_STAGE13_SOURCE_ROOT','NSL_STAGE13_BASE','NSL_STAGE13_DLC_DIR','NSL_STAGE13_QA_ROOT')
$previous = @{}
foreach ($key in $vars) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key,'Process')
}
$project = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
try {
    $env:NSL_STAGE13_SOURCE_ROOT = $source
    $env:NSL_STAGE13_BASE = $base[0].FullName
    $env:NSL_STAGE13_DLC_DIR = $dlcRoot
    $env:NSL_STAGE13_QA_ROOT = $qa
    Push-Location -LiteralPath $project
    try {
        & cargo test --locked stage27_real_dlc_mid_write_owned_child_crash_and_retry -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) {
            throw "Stage27 owned-child mid-write crash test failed: exit=$LASTEXITCODE"
        }
        Write-Output 'STAGE27_REAL_MIDWRITE_RECOVERY_SCRIPT=PASS'
    } finally {
        Pop-Location
    }
} finally {
    foreach ($key in $vars) {
        [Environment]::SetEnvironmentVariable($key,$previous[$key],'Process')
    }
}
