param(
    [Parameter(Mandatory = $true)][string]$PackageZip,
    [Parameter(Mandatory = $true)][string]$StagingDir
)

# Inspect the archive itself, not merely the folder used to create it. Never
# extract an untrusted archive over user data to verify a release.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$staging = (Resolve-Path -LiteralPath $StagingDir -ErrorAction Stop).Path.TrimEnd('\', '/')
$archive = (Resolve-Path -LiteralPath $PackageZip -ErrorAction Stop).Path
$allowed = @(
    'nsl.exe', 'nsl-manifest.json',
    'licenses/memmap2-MIT.txt',
    'licenses/nsrp-MIT.txt',
    'licenses/zstd-BSD-3-Clause.txt',
    'licenses/zstd-sys-BSD-2-Clause.txt',
    'licenses/zstd-sys-BSD-3-Clause.txt',
    'titledb/catalog.json', 'titledb/KR.ko.json',
    'titledb/titles.json', 'titledb/versions.json'
)
$expected = [System.Collections.Generic.Dictionary[string, string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($file in Get-ChildItem -LiteralPath $staging -Recurse -File) {
    $relative = $file.FullName.Substring($staging.Length + 1).Replace('\', '/')
    if ($allowed -cnotcontains $relative) {
        throw "Unexpected file in release staging: $relative"
    }
    $expected.Add($relative, $file.FullName)
}
foreach ($required in @(
    'nsl.exe', 'nsl-manifest.json',
    # Older r22 and earlier ZIPs do not bundle the new NSRP attribution.
    # Keep them verifiable; new package.ps1 always includes the notice.
    'licenses/memmap2-MIT.txt', 'licenses/zstd-BSD-3-Clause.txt',
    'licenses/zstd-sys-BSD-2-Clause.txt', 'licenses/zstd-sys-BSD-3-Clause.txt'
)) {
    if (-not $expected.ContainsKey($required)) {
        throw "Missing required release file: $required"
    }
}

$manifest = Get-Content -LiteralPath (Join-Path $staging 'nsl-manifest.json') -Raw |
    ConvertFrom-Json
if ($manifest.application -cne 'NSL' -or
    $manifest.platform -cne 'windows-x86_64' -or
    $manifest.commit -cnotmatch '^[0-9a-fA-F]{40}$') {
    throw 'Release manifest is missing a valid build commit.'
}
if ($env:GITHUB_SHA -and $manifest.commit -cne $env:GITHUB_SHA.ToLowerInvariant()) {
    throw 'Release manifest commit does not match the workflow commit.'
}
$executableHash = (Get-FileHash -LiteralPath (Join-Path $staging 'nsl.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
if ($manifest.sha256.'nsl.exe' -cne $executableHash) {
    throw 'Release manifest SHA-256 does not match the staged nsl.exe.'
}

$zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
$seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
try {
    foreach ($entry in $zip.Entries) {
        $name = $entry.FullName.Replace('\', '/')
        if ([string]::IsNullOrEmpty($entry.Name)) {
            if (@('icons/', 'screenshots/', 'licenses/', 'titledb/') -cnotcontains $name) {
                throw "Unexpected folder entry in release: $name"
            }
            continue
        }
        if (-not $expected.ContainsKey($name) -or $allowed -cnotcontains $name) {
            throw "Unexpected release archive entry: $name"
        }
        if (-not $seen.Add($name)) {
            throw "Duplicate release archive entry: $name"
        }
        # Stream-decompress and hash, rather than trusting a ZIP file list or
        # loading the multi-hundred-megabyte title database into memory.
        $stream = $entry.Open()
        $hasher = [System.Security.Cryptography.SHA256]::Create()
        try {
            $digest = ([System.BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        } finally {
            $hasher.Dispose()
            $stream.Dispose()
        }
        $onDisk = (Get-FileHash -LiteralPath $expected[$name] -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($digest -cne $onDisk) {
            throw "Release ZIP entry differs from staging: $name"
        }
    }
} finally {
    $zip.Dispose()
}
if ($seen.Count -ne $expected.Count) {
    throw "Release ZIP has $($seen.Count) files but staging requires $($expected.Count)."
}
Write-Host "Verified release ZIP: $($seen.Count) allowlisted files, SHA-256 and manifest OK."
