param(
    [Parameter(Mandatory=$true)][string]$ArchivePath,
    [Parameter(Mandatory=$true)][string]$DestinationPath
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$root = [IO.Path]::GetFullPath($DestinationPath)
$prefix = $root.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$totalSize = [long]0
$maxExpandedBytes = [long](768 * 1024 * 1024)
$zip = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
try {
    foreach ($entry in $zip.Entries) {
        $relative = $entry.FullName.Replace('/', [IO.Path]::DirectorySeparatorChar)
        if ([IO.Path]::IsPathRooted($relative)) {
            throw "Absolute path in update archive: $relative"
        }
        $destination = [IO.Path]::GetFullPath([IO.Path]::Combine($root, $relative))
        if (-not $destination.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Path traversal in update archive: $relative"
        }
        $totalSize += $entry.Length
        if ($totalSize -gt $maxExpandedBytes) {
            throw 'Update archive exceeds the maximum expanded size of 768 MB'
        }
    }
} finally {
    $zip.Dispose()
}
Expand-Archive -LiteralPath $ArchivePath -DestinationPath $DestinationPath -Force
