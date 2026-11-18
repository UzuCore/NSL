# Builds the distributable Rust package.
#
# The release zip must never contain local user data: no library.db rows, no
# config.json, no prod.keys/title.keys, and no icon/screenshot caches. The
# package is assembled in a clean staging folder so anything left inside
# dist-rust by a previous local run cannot leak into the uploaded asset.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dist = Join-Path $root 'dist-rust'
$staging = Join-Path $root 'build\rust-package'

$cargoToml = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw 'Could not read the Rust package version from Cargo.toml.'
}
$version = $Matches[1]

$required = @(
    @{ From = (Join-Path $root 'target\release\nsl.exe'); To = 'nsl.exe' },
    @{ From = (Join-Path $root 'bin\nstool.exe');         To = 'bin\nstool.exe' },
    @{ From = (Join-Path $root 'bin\hactool.exe');        To = 'bin\hactool.exe' }
)
foreach ($item in $required) {
    if (-not (Test-Path -LiteralPath $item.From)) {
        throw "Required build input is missing: $($item.From)"
    }
}

# Local title database is optional; ship whatever JSON is available.
$titleDbSource = Join-Path $root 'titledb'
$titleDbFiles = if (Test-Path -LiteralPath $titleDbSource) {
    Get-ChildItem -LiteralPath $titleDbSource -Filter '*.json' -File
} else {
    @()
}

if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'bin') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'titledb') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'icons') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'screenshots') -Force | Out-Null

Copy-Item -LiteralPath (Join-Path $root 'target\release\nsl.exe') -Destination (Join-Path $staging 'nsl.exe')
Copy-Item -LiteralPath (Join-Path $root 'bin\nstool.exe') -Destination (Join-Path $staging 'bin\nstool.exe')
Copy-Item -LiteralPath (Join-Path $root 'bin\hactool.exe') -Destination (Join-Path $staging 'bin\hactool.exe')
foreach ($file in $titleDbFiles) {
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $staging ('titledb\' + $file.Name))
}

# The app creates its own empty database on first launch. A packaged database
# would overwrite an existing user's entire library during an in-place update.

$manifest = [ordered]@{
    application = 'NSL-Rust'
    version = $version
    platform = 'windows-x86_64'
    sha256 = [ordered]@{
        'nsl.exe' = (Get-FileHash -LiteralPath (Join-Path $staging 'nsl.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        'bin/nstool.exe' = (Get-FileHash -LiteralPath (Join-Path $staging 'bin/nstool.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        'bin/hactool.exe' = (Get-FileHash -LiteralPath (Join-Path $staging 'bin/hactool.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    }
} | ConvertTo-Json
$utf8 = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText((Join-Path $staging 'nsl-rust-manifest.json'), $manifest, $utf8)

# Mirror the staged tree into dist-rust so a local test run still has a complete
# folder, then upload only the freshly built files.
if (-not (Test-Path -LiteralPath $dist)) {
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
}
Copy-Item -Path (Join-Path $staging '*') -Destination $dist -Recurse -Force

$forbidden = @('prod.keys', 'title.keys')
foreach ($name in $forbidden) {
    $leak = Join-Path $staging $name
    if (Test-Path -LiteralPath $leak) {
        throw "Refusing to package key material: $name"
    }
}

$archive = Join-Path $dist 'nsl-rust-win64.zip'
if (Test-Path -LiteralPath $archive) {
    Remove-Item -LiteralPath $archive -Force
}
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archive -CompressionLevel Optimal

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
try {
    $names = $zip.Entries | ForEach-Object { $_.FullName -replace '\\', '/' }
} finally {
    $zip.Dispose()
}
foreach ($forbiddenName in @('prod.keys', 'title.keys', 'config.json', 'nsl-keys.json', 'library.db', 'library.db-wal', 'library.db-shm')) {
    if ($names -contains $forbiddenName) {
        throw "Release archive contains forbidden local file: $forbiddenName"
    }
}
if ($names -notcontains 'nsl.exe' -or $names -notcontains 'nsl-rust-manifest.json') {
    throw 'Release archive is missing the executable or the package manifest.'
}
Write-Host ("Packaged {0} files ({1:N1} MB) as {2}" -f $names.Count, ((Get-Item -LiteralPath $archive).Length / 1MB), $archive)
