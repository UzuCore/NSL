# Isolated regression for the ZIP verifier. Uses invented, tiny files only;
# never reads production titles, user keys, SQLite data or game content.
$ErrorActionPreference = 'Stop'
$verify = Join-Path $PSScriptRoot 'verify-package.ps1'
$temp = Join-Path ([System.IO.Path]::GetTempPath()) ('nsl-package-qa-' + [guid]::NewGuid().ToString('N'))
$stage = Join-Path $temp 'stage'
$zipPath = Join-Path $temp 'sample.zip'
$utf8 = [System.Text.UTF8Encoding]::new($false)

function New-SampleZip {
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zipPath -CompressionLevel Fastest
}

function Assert-Invalid([string]$scenario) {
    $rejected = $false
    try {
        & $verify -PackageZip $zipPath -StagingDir $stage | Out-Null
    } catch {
        $rejected = $true
    }
    if (-not $rejected) { throw "Verifier accepted invalid case: $scenario" }
    Write-Host "PASS: rejected $scenario"
}

try {
    New-Item -ItemType Directory -Path (Join-Path $stage 'licenses') -Force | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl.exe'), 'fake test executable', $utf8)
    foreach ($name in @('memmap2-MIT.txt', 'nsrp-MIT.txt', 'zstd-BSD-3-Clause.txt', 'zstd-sys-BSD-2-Clause.txt', 'zstd-sys-BSD-3-Clause.txt')) {
        [System.IO.File]::WriteAllText((Join-Path $stage ('licenses\' + $name)), 'mock license notice', $utf8)
    }
    $exeHash = (Get-FileHash -LiteralPath (Join-Path $stage 'nsl.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifest = @{ application = 'NSL'; version = '0.0.0-test'; commit = ('a' * 40); platform = 'windows-x86_64'; sha256 = @{ 'nsl.exe' = $exeHash } } | ConvertTo-Json
    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl-manifest.json'), $manifest, $utf8)
    New-SampleZip
    & $verify -PackageZip $zipPath -StagingDir $stage

    # The older official release remains valid although it predates the
    # MIT notice now included by the new packager.
    Remove-Item -LiteralPath (Join-Path $stage 'licenses\nsrp-MIT.txt') -Force
    New-SampleZip
    & $verify -PackageZip $zipPath -StagingDir $stage
    Write-Host 'PASS: compatible with legacy release ZIP without NSRP notice'
    [System.IO.File]::WriteAllText((Join-Path $stage 'licenses\nsrp-MIT.txt'), 'mock license notice', $utf8)
    New-SampleZip

    $invalidManifest = @{ application = 'NSL'; version = '0.0.0-test'; commit = 'not-a-commit'; platform = 'windows-x86_64'; sha256 = @{ 'nsl.exe' = $exeHash } } | ConvertTo-Json
    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl-manifest.json'), $invalidManifest, $utf8)
    New-SampleZip
    Assert-Invalid 'invalid release commit'
    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl-manifest.json'), $manifest, $utf8)
    New-SampleZip

    # Unexpected release content, including a nested config file, must fail.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Update)
    try {
        $entry = $zip.CreateEntry('titledb/config.json')
        $writer = [System.IO.StreamWriter]::new($entry.Open())
        try { $writer.Write('{"token":"not-for-release"}') } finally { $writer.Dispose() }
    } finally { $zip.Dispose() }
    Assert-Invalid 'unexpected nested JSON'

    New-SampleZip
    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl.exe'), 'changed after ZIP creation', $utf8)
    Assert-Invalid 'staged executable / manifest mismatch'

    [System.IO.File]::WriteAllText((Join-Path $stage 'nsl.exe'), 'fake test executable', $utf8)
    [System.IO.File]::WriteAllText((Join-Path $stage 'licenses\memmap2-MIT.txt'), 'changed after ZIP creation', $utf8)
    Assert-Invalid 'ZIP content hash mismatch'
    Write-Host 'PASS: all package verifier regression cases'
} finally {
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force }
}
