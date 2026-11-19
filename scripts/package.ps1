param(
    [string]$ExecutableOverride = ''
)

# Builds the distributable NSL package.
#
# The release zip must never contain local user data: no library.db rows, no
# config.json, no prod.keys/title.keys, and no icon/screenshot caches. The
# package is assembled in a clean staging folder so anything left inside
# dist by a previous local run cannot leak into the uploaded asset.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
$staging = Join-Path $root 'build\package'

$cargoToml = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw 'Could not read the package version from Cargo.toml.'
}
$version = $Matches[1]
$commit = (& git -C $root rev-parse HEAD).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') {
    throw 'Could not identify the Git commit for the release.'
}
if ($env:GITHUB_SHA -and $commit -cne $env:GITHUB_SHA.ToLowerInvariant()) {
    throw 'Checkout HEAD differs from the GitHub workflow commit.'
}
$releaseExe = if ([string]::IsNullOrWhiteSpace($ExecutableOverride)) {
    Join-Path $root 'target\release\nsl.exe'
} else {
    (Resolve-Path -LiteralPath $ExecutableOverride -ErrorAction Stop).Path
}

$required = @(
    @{ From = $releaseExe; To = 'nsl.exe' },
    @{ From = (Join-Path $root 'licenses\memmap2-MIT.txt'); To = 'licenses\memmap2-MIT.txt' },
    @{ From = (Join-Path $root 'licenses\zstd-BSD-3-Clause.txt'); To = 'licenses\zstd-BSD-3-Clause.txt' },
    @{ From = (Join-Path $root 'licenses\zstd-sys-BSD-2-Clause.txt'); To = 'licenses\zstd-sys-BSD-2-Clause.txt' },
    @{ From = (Join-Path $root 'licenses\zstd-sys-BSD-3-Clause.txt'); To = 'licenses\zstd-sys-BSD-3-Clause.txt' }
)
foreach ($item in $required) {
    if (-not (Test-Path -LiteralPath $item.From)) {
        throw "Required build input is missing: $($item.From)"
    }
}

# Ship only the four known public title-data files. An arbitrary JSON added
# under titledb/ must never become part of a public release by accident.
$titleDbSource = Join-Path $root 'titledb'
$titleDbFiles = @(
    foreach ($name in @('catalog.json', 'KR.ko.json', 'titles.json', 'versions.json')) {
        $source = Join-Path $titleDbSource $name
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Get-Item -LiteralPath $source
        }
    }
)
if (-not $titleDbFiles) {
    Write-Warning 'No public title database JSON found; packaging without title data.'
}

if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'titledb') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'icons') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'screenshots') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $staging 'licenses') -Force | Out-Null

Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $staging 'nsl.exe')
foreach ($notice in @('memmap2-MIT.txt','zstd-BSD-3-Clause.txt','zstd-sys-BSD-2-Clause.txt','zstd-sys-BSD-3-Clause.txt')) {
    Copy-Item -LiteralPath (Join-Path $root ('licenses\' + $notice)) -Destination (Join-Path $staging ('licenses\' + $notice))
}
foreach ($file in $titleDbFiles) {
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $staging ('titledb\' + $file.Name))
}

# The app creates its own empty database on first launch. A packaged database
# would overwrite an existing user's entire library during an in-place update.

$manifest = [ordered]@{
    application = 'NSL'
    version = $version
    commit = $commit
    platform = 'windows-x86_64'
    sha256 = [ordered]@{
        'nsl.exe' = (Get-FileHash -LiteralPath (Join-Path $staging 'nsl.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    }
} | ConvertTo-Json
$utf8 = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText((Join-Path $staging 'nsl-manifest.json'), $manifest, $utf8)

# Validate a candidate ZIP before changing any existing distribution files.
# This also rejects nested user configuration, key material or unknown JSON.
$candidate = Join-Path $root 'build\nsl-win64.verify.zip'
if (Test-Path -LiteralPath $candidate) {
    Remove-Item -LiteralPath $candidate -Force
}
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $candidate -CompressionLevel Optimal
& (Join-Path $root 'scripts\verify-package.ps1') -PackageZip $candidate -StagingDir $staging

# Only verified release content may be mirrored into dist. Never copy a
# user database, keys, settings, media or downloads from the project root.
if (-not (Test-Path -LiteralPath $dist)) {
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
}
# The local dist folder may already be an active user's portable install.
# Refresh only the application and public notices; keep its title data,
# icons, screenshots, SQLite database, keys and configuration untouched.
Copy-Item -LiteralPath (Join-Path $staging 'nsl.exe') -Destination (Join-Path $dist 'nsl.exe') -Force
Copy-Item -LiteralPath (Join-Path $staging 'nsl-manifest.json') -Destination (Join-Path $dist 'nsl-manifest.json') -Force
New-Item -ItemType Directory -Path (Join-Path $dist 'licenses') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $dist 'titledb') -Force | Out-Null
foreach ($notice in @('memmap2-MIT.txt','zstd-BSD-3-Clause.txt','zstd-sys-BSD-2-Clause.txt','zstd-sys-BSD-3-Clause.txt')) {
    Copy-Item -LiteralPath (Join-Path $staging ('licenses\' + $notice)) -Destination (Join-Path $dist ('licenses\' + $notice)) -Force
}
foreach ($file in $titleDbFiles) {
    $localTitle = Join-Path $dist ('titledb\' + $file.Name)
    if (-not (Test-Path -LiteralPath $localTitle)) {
        Copy-Item -LiteralPath (Join-Path $staging ('titledb\' + $file.Name)) -Destination $localTitle
    }
}
foreach ($folder in @('icons','screenshots')) {
    New-Item -ItemType Directory -Path (Join-Path $dist $folder) -Force | Out-Null
}

# Remove only known old distribution tools; preserve all unrelated user files.
foreach ($obsolete in @('bin\nstool.exe', 'bin\hactool.exe', 'licenses\hactool-ISC.txt')) {
    $old = Join-Path $dist $obsolete
    if (Test-Path -LiteralPath $old -PathType Leaf) {
        Remove-Item -LiteralPath $old -Force
    }
}

$archive = Join-Path $dist 'nsl-win64.zip'
Move-Item -LiteralPath $candidate -Destination $archive -Force

# Keep the build output's manifest and checksum in sync with the published ZIP.
# Do not overwrite target/release/titledb: those files may be live user data.
Copy-Item -LiteralPath (Join-Path $staging 'nsl-manifest.json') -Destination (Join-Path $root 'target\release\nsl-manifest.json') -Force
$archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
$checksumFile = "SHA256SUMS-$version.txt"
$checksumText = "$archiveHash  nsl-win64.zip`n"
[System.IO.File]::WriteAllText((Join-Path $root ('target\release\' + $checksumFile)), $checksumText, $utf8)
Copy-Item -LiteralPath (Join-Path $root ('target\release\' + $checksumFile)) -Destination (Join-Path $dist $checksumFile) -Force

foreach ($relative in @('nsl.exe', 'nsl-manifest.json')) {
    $source = Join-Path $staging $relative
    $published = Join-Path $dist $relative
    if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -cne (Get-FileHash -LiteralPath $published -Algorithm SHA256).Hash) {
        throw "Published release file does not match verified staging: $relative"
    }
}
# Retire only recognized artifacts from the previous packaging convention.
# This runs after the new ZIP and executable have passed verification.
foreach ($legacy in @('nsl-rust-win64.zip', 'nsl-rust-manifest.json')) {
    $old = Join-Path $dist $legacy
    if (Test-Path -LiteralPath $old -PathType Leaf) {
        Remove-Item -LiteralPath $old -Force
    }
}
$oldTargetManifest = Join-Path $root 'target\release\nsl-rust-manifest.json'
if (Test-Path -LiteralPath $oldTargetManifest -PathType Leaf) {
    Remove-Item -LiteralPath $oldTargetManifest -Force
}
Write-Host ("Packaged {0} files ({1:N1} MB) as {2}" -f (6 + $titleDbFiles.Count), ((Get-Item -LiteralPath $archive).Length / 1MB), $archive)
$global:LASTEXITCODE = 0
