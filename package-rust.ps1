$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dist = Join-Path $root 'dist-rust'
$cargoToml = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw 'Could not read the Rust package version from Cargo.toml.'
}
$version = $Matches[1]
$manifest = [ordered]@{
    application = 'NSL-Rust'
    version = $version
    platform = 'windows-x86_64'
} | ConvertTo-Json
$utf8 = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText((Join-Path $dist 'nsl-rust-manifest.json'), $manifest, $utf8)
$archive = Join-Path $dist 'nsl-rust-win64.zip'
if (Test-Path -LiteralPath $archive) {
    Remove-Item -LiteralPath $archive -Force
}
Compress-Archive -Path (Join-Path $dist '*') -DestinationPath $archive -CompressionLevel Optimal
