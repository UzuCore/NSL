# Isolated retail BKTR QA. Never mutate E:\nsw source or existing emulator.
# The reference executable is read-only and invoked against private test NCAs.
param(
    [switch]$Release,
    [switch]$LibHacFirst
)
$ErrorActionPreference = 'Stop'
$origin = 'E:\nsw\BALL x PIT [010086A022444000][B+U262144].nsz'
$expectedHash = 'A2C746450751F1977A5F8422158F4FF2226A2B7037147FFCB0311472DBC2E289'
$qaBase = 'D:\NSW.dev\NSL_STAGE38_SAFETY_20261122'
$srcKeys = 'D:\NSW.dev\NSL'
$reference = 'D:\NSW.dev\NSRP\bin\hactoolnet.exe'
if (-not (Test-Path -LiteralPath $origin -PathType Leaf)) { throw 'Verified Stage41 source not present' }
foreach ($needed in @((Join-Path $srcKeys 'prod.keys'),(Join-Path $srcKeys 'title.keys'),$reference)) {
    if (-not (Test-Path -LiteralPath $needed -PathType Leaf)) { throw 'Required local input missing' }
}
if ((Get-Item -LiteralPath $origin).Length -ne 454511886) { throw 'Source size differs from Stage41 original' }
$originalHash = (Get-FileHash -LiteralPath $origin -Algorithm SHA256).Hash
if ($originalHash -ne $expectedHash) { throw 'Stage41 original source hash mismatch; refuse to use it' }
if (-not (Test-Path -LiteralPath $qaBase -PathType Container)) {
    $null = New-Item -ItemType Directory -Path $qaBase
}
$qaDir = Join-Path $qaBase ('stage38-owned-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $qaDir
$marker = Join-Path $qaDir '.stage38-owned'
$null = New-Item -ItemType File -Path $marker
try {
    $sample = Join-Path $qaDir 'legacy_sample.nsz'
    Copy-Item -LiteralPath $origin -Destination $sample -ErrorAction Stop
    foreach ($name in @('prod.keys','title.keys')) {
        Copy-Item -LiteralPath (Join-Path $srcKeys $name) -Destination (Join-Path $qaDir $name) -ErrorAction Stop
    }
    if ((Get-FileHash -LiteralPath $sample -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'Private copy SHA-256 does not match original'
    }
    $env:NSRP_ROOT = $qaDir
    $env:NSRP_STAGE41_VERIFY_ROMS = '1'
    $env:NSRP_STAGE41_HACTOOLNET_REFERENCE = $reference
    if ($LibHacFirst) { $env:NSRP_STAGE52_BKTR_ORDER = 'libhac_first' }
    $cargoArgs = @('test','--locked','-p','nsrp-core')
    if ($Release) { $cargoArgs += '--release' }
    $cargoArgs += @('stage38_real_hashed_bktr_native_libhac_exact_sha','--','--ignored','--nocapture','--test-threads=1')
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "Independent Stage38 Rust test failed with $LASTEXITCODE" }
    if ((Get-FileHash -LiteralPath $origin -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'Original input changed after private BKTR test'
    }
    Write-Output "STAGE38_ORIGINAL_SHA256=$expectedHash"
    Write-Output 'STAGE38_RETAIL_BKTR_LIBHAC_EXACT_SHA=PASS'
} finally {
    Remove-Item Env:NSRP_ROOT,Env:NSRP_STAGE41_VERIFY_ROMS,Env:NSRP_STAGE41_HACTOOLNET_REFERENCE,Env:NSRP_STAGE52_BKTR_ORDER -ErrorAction SilentlyContinue
    if ((Test-Path -LiteralPath $marker -PathType Leaf) -and
        ((Split-Path -Leaf $qaDir).StartsWith('stage38-owned-')) -and
        ((Split-Path -Parent $qaDir).Equals($qaBase, [StringComparison]::OrdinalIgnoreCase))) {
        Remove-Item -LiteralPath $qaDir -Recurse -Force -ErrorAction Stop
        Write-Output 'STAGE38_OWNED_QA_CLEANUP=YES'
    }
}
