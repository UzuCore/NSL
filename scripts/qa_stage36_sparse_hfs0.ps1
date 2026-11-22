# Stage36: create only an owned, temporary NTFS sparse gamecard index fixture.
# The file is logically 36 GiB but physically small; no game or emulator access.
param([string]$QaRoot = 'E:\NSL_QA_20261120')
$ErrorActionPreference = 'Stop'
$expected = 'E:\NSL_QA_20261120'
if (-not $QaRoot.TrimEnd('\').Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Stage36 refuses a non-approved QA root.'
}
$volume = Get-Volume -DriveLetter E -ErrorAction Stop
if ($volume.FileSystem -ne 'NTFS') { throw 'Stage36 requires an NTFS E: volume.' }
if (-not (Test-Path -LiteralPath $QaRoot -PathType Container)) {
    throw 'Stage36 isolated QA root is missing.'
}
$directory = Join-Path $QaRoot ('stage36-sparse-hfs0-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $directory -ErrorAction Stop
$marker = Join-Path $directory '.stage36-owned'
$fixture = Join-Path $directory 'sparse_36gib.xci'
$null = New-Item -ItemType File -Path $marker -ErrorAction Stop
try {
    $stream = [System.IO.File]::Open($fixture, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $stream.Dispose()
    & fsutil sparse setflag $fixture
    if ($LASTEXITCODE -ne 0) { throw "Could not mark owned Stage36 file as NTFS sparse: $LASTEXITCODE" }
    $attributes = [IO.File]::GetAttributes($fixture)
    if (($attributes -band [IO.FileAttributes]::SparseFile) -eq 0) {
        throw 'NTFS did not enable sparse attribute. Refusing set-EOF.'
    }
    & fsutil file seteof $fixture 38654705664
    if ($LASTEXITCODE -ne 0) { throw "Could not set logical 36 GiB sparse length: $LASTEXITCODE" }
    $stream = [System.IO.File]::Open($fixture, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $writer = [IO.BinaryWriter]::new($stream)
        $null = $stream.Seek(512, [IO.SeekOrigin]::Begin)
        $writer.Write([Text.Encoding]::ASCII.GetBytes('HFS0'))
        $writer.Write([uint32]1)
        $writer.Write([uint32]8)
        $writer.Write([uint32]0)
        $writer.Write([uint64](34GB))
        $writer.Write([uint64](1MB))
        $writer.Write([uint32]0)
        $writer.Write([uint32]0)
        $writer.Write([byte[]]::new(40))
        $writer.Write([Text.Encoding]::ASCII.GetBytes("big.nca`0"))
        $absolute = [long](600 + 34GB)
        $null = $stream.Seek($absolute, [IO.SeekOrigin]::Begin)
        $writer.Write([Text.Encoding]::ASCII.GetBytes('STAGE36_SPARSE!'))
        $null = $stream.Seek($absolute + 1MB - 4, [IO.SeekOrigin]::Begin)
        $writer.Write([Text.Encoding]::ASCII.GetBytes('TAIL'))
        $writer.Flush()
        $stream.Flush($true)
    } finally { $stream.Dispose() }
    if ((Get-Item -LiteralPath $fixture).Length -ne 38654705664) {
        throw 'Unexpected sparse fixture logical size.'
    }
    $env:NSL_STAGE36_SPARSE_XCI = $fixture
    & cargo test --locked -p nsrp-core stage36_real_ntfs_sparse_36gib_hfs0 -- --ignored --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "Stage36 Rust sparse test failed: $LASTEXITCODE" }
    Write-Output 'STAGE36_REAL_NTFS_SPARSE_36GIB=PASS'
} finally {
    Remove-Item Env:NSL_STAGE36_SPARSE_XCI -ErrorAction SilentlyContinue
    if ((Test-Path -LiteralPath $marker -PathType Leaf) -and
        (Split-Path -Leaf $directory).StartsWith('stage36-sparse-hfs0-')) {
        Remove-Item -LiteralPath $fixture -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $directory -ErrorAction SilentlyContinue
    }
}
