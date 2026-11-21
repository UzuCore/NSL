# Stage 28: real NSZ safe publication-before-journal crash regression.
# Original game/keys/saves remain read-only; this script launches only a
# test-owned child into an auto-cleaned isolated QA folder.
param(
    [string]$SourceRoot = 'E:\NSW',
    [string]$QaRoot = 'E:\NSL_QA_20261120',
    [string]$BaseId = '0100800015926000',
    [string]$DlcId = '010080001592701D'
)
$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $SourceRoot).ProviderPath.TrimEnd('\')
$qa = (Resolve-Path -LiteralPath $QaRoot).ProviderPath.TrimEnd('\')
if ($source.Equals($qa, [StringComparison]::OrdinalIgnoreCase) -or
    $qa.StartsWith(($source + '\'), [StringComparison]::OrdinalIgnoreCase) -or
    $source.StartsWith(($qa + '\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Source and scratch must have disjoint roots.'
}
if ($BaseId -ne '0100800015926000' -or $DlcId -ne '010080001592701D') {
    throw 'Refuse any unverified real-content Title IDs.'
}
$base = @(Get-ChildItem -LiteralPath $source -File -Filter "*$BaseId*.nsz")
$dlcDir = Join-Path $source '_dlc'
$dlc = @(Get-ChildItem -LiteralPath $dlcDir -File -Filter "*$DlcId*.nsp")
if ($base.Count -ne 1 -or $dlc.Count -ne 1) {
    throw 'Expected exactly one authorized base NSZ and one DLC NSP.'
}
if ((Get-Item -LiteralPath $qa).PSDrive.Free -lt 16GB) {
    throw 'At least 16 GiB available on isolated QA scratch required.'
}
$keys = @('NSL_STAGE13_SOURCE_ROOT','NSL_STAGE13_BASE','NSL_STAGE13_DLC_DIR','NSL_STAGE13_QA_ROOT')
$old = @{}
foreach($key in $keys) {$old[$key] = [Environment]::GetEnvironmentVariable($key,'Process')}
try {
    $env:NSL_STAGE13_SOURCE_ROOT = $source
    $env:NSL_STAGE13_BASE = $base[0].FullName
    $env:NSL_STAGE13_DLC_DIR = $dlcDir
    $env:NSL_STAGE13_QA_ROOT = $qa
    Push-Location -LiteralPath ((Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath)
    try {
        & cargo test --locked stage28_real_dlc_postpublish_prejournal_crash_recovers_without_rewriting -- --ignored --nocapture --test-threads=1
        if($LASTEXITCODE -ne 0) {
            throw "Stage28 real publish-before-journal recovery failed: $LASTEXITCODE"
        }
        Write-Output 'STAGE28_REAL_POSTPUBLICATION_RECOVERY_SCRIPT=PASS'
    } finally { Pop-Location }
} finally {
    foreach($key in $keys) {[Environment]::SetEnvironmentVariable($key,$old[$key],'Process')}
}
