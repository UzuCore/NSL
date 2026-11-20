param(
    [Parameter(Mandatory = $true)][string]$Game,
    [Parameter(Mandatory = $true)][string]$TicketNsp,
    [Parameter(Mandatory = $true)][string]$EdenCli,
    [Parameter(Mandatory = $true)][string]$ProdKeys,
    [Parameter(Mandatory = $true)][string]$QaDir,
    [ValidateRange(10, 120)][int]$ObserveSeconds = 24,
    [ValidatePattern('^[a-zA-Z0-9_-]{1,32}$')][string]$RunLabel = 'smoke'
)

$ErrorActionPreference = 'Stop'
$qa = (Resolve-Path -LiteralPath $QaDir).Path
$gamePath = (Resolve-Path -LiteralPath $Game).Path
$ticketPath = (Resolve-Path -LiteralPath $TicketNsp).Path
$edenPath = (Resolve-Path -LiteralPath $EdenCli).Path
$keysPath = (Resolve-Path -LiteralPath $ProdKeys).Path
if (-not $gamePath.StartsWith($qa + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The loader-only sample must be under the explicitly isolated QA directory'
}
if ([IO.Path]::GetExtension($gamePath) -ine '.nsz') { throw 'Only isolated NSZ files are supported' }
if (@(Get-Process -Name eden, eden-cli, yuzu, ryujinx -ErrorAction SilentlyContinue).Count -gt 0) {
    throw 'Another emulator is active: refusing to interfere with it'
}

$private = Join-Path $qa ('eden-loader-private-' + [Guid]::NewGuid().ToString('N'))
$exe = Join-Path $private 'eden-cli.exe'
$keydir = Join-Path $private 'user\keys'
$log = Join-Path $private 'user\log\eden_log.txt'
$capture = Join-Path $qa ('eden_loader_' + $RunLabel + '.png')
$reportPath = Join-Path $qa ('eden_loader_' + $RunLabel + '.json')
if ((Test-Path -LiteralPath $capture) -or (Test-Path -LiteralPath $reportPath)) {
    throw 'QA evidence already exists: refusing to overwrite it'
}

$originalAppdata = $env:APPDATA
$originalLocalappdata = $env:LOCALAPPDATA
$process = $null
try {
    foreach ($relative in @('user\keys', 'user\log', 'user\sdmc', 'user\load', 'user\dump', 'user\tas', 'user\nand', 'user\cache', 'appdata', 'localappdata')) {
        New-Item -ItemType Directory -Path (Join-Path $private $relative) -Force | Out-Null
    }
    Copy-Item -LiteralPath $edenPath -Destination $exe -ErrorAction Stop
    Copy-Item -LiteralPath $keysPath -Destination (Join-Path $keydir 'prod.keys') -ErrorAction Stop

    # Read exactly the PFS0 index and a ticket's 16-byte encrypted title key.
    # No game or key file is opened for writing, and no secret is printed.
    $stream = [IO.File]::Open($ticketPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $ticketKeys = [ordered]@{}
    try {
        $reader = [IO.BinaryReader]::new($stream)
        if ([Text.Encoding]::ASCII.GetString($reader.ReadBytes(4)) -ne 'PFS0') { throw 'The patch is not a PFS0 NSP' }
        $count = $reader.ReadUInt32()
        $stringsSize = $reader.ReadUInt32()
        $null = $reader.ReadUInt32()
        if ($count -eq 0 -or $count -gt 16384 -or $stringsSize -gt 67108864) { throw 'Unsafe PFS0 index bounds' }
        $entries = @()
        for ($i = 0; $i -lt $count; $i++) {
            $entries += [pscustomobject]@{ Offset = $reader.ReadUInt64(); Size = $reader.ReadUInt64(); NameOffset = $reader.ReadUInt32() }
            $null = $reader.ReadUInt32()
        }
        $strings = $reader.ReadBytes($stringsSize)
        $dataStart = $stream.Position
        foreach ($entry in $entries) {
            if ($entry.NameOffset -ge $strings.Length) { throw 'PFS0 filename offset outside string table' }
            $start = [int]$entry.NameOffset
            $end = $start
            while ($end -lt $strings.Length -and $strings[$end] -ne 0) { $end++ }
            if ($end -ge $strings.Length) { throw 'PFS0 filename is not terminated' }
            $name = [Text.Encoding]::UTF8.GetString($strings, $start, $end - $start)
            if ($name -notmatch '^([0-9a-fA-F]{32})\.tik$' -or $entry.Size -lt 400 -or $entry.Size -gt 1048576) { continue }
            $rightsId = $Matches[1].ToUpperInvariant()
            $absolute = [uint64]$dataStart + $entry.Offset + 0x180
            if ($absolute + 16 -gt [uint64]$stream.Length) { throw 'PFS0 ticket extends beyond source file' }
            $null = $stream.Seek([int64]$absolute, [IO.SeekOrigin]::Begin)
            $key = $reader.ReadBytes(16)
            if ($key.Length -ne 16) { throw 'Truncated PFS0 ticket' }
            if (@($key | Where-Object { $_ -ne 0 }).Count -eq 0) { continue }
            if (-not $ticketKeys.Contains($rightsId)) {
                $ticketKeys[$rightsId] = [BitConverter]::ToString($key).Replace('-', '')
            }
        }
    } finally { $stream.Dispose() }
    if ($ticketKeys.Count -eq 0) { throw 'The isolated patch does not contain a usable encrypted ticket key' }
    $lines = foreach ($keyId in $ticketKeys.Keys) { $keyId + ' = ' + $ticketKeys[$keyId] }
    [IO.File]::WriteAllLines((Join-Path $keydir 'title.keys'), [string[]]$lines, [Text.UTF8Encoding]::new($false))

    $hashBefore = (Get-FileHash -LiteralPath $gamePath -Algorithm SHA256).Hash
    $env:APPDATA = Join-Path $private 'appdata'
    $env:LOCALAPPDATA = Join-Path $private 'localappdata'
    $process = Start-Process -FilePath $exe -ArgumentList @('--game', ('"' + $gamePath + '"')) -WorkingDirectory $private -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $private 'stdout.log') -RedirectStandardError (Join-Path $private 'stderr.log')
    Start-Sleep -Seconds $ObserveSeconds
    $process.Refresh()
    if ($process.HasExited -or $process.MainWindowHandle -eq [IntPtr]::Zero) { throw 'Eden exited or failed to open its real renderer window' }

    Add-Type -AssemblyName System.Drawing
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NslLoaderSmokeWindow {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
}
'@
    [IntPtr]$hwnd = [IntPtr]$process.MainWindowHandle
    [uint32]$owner = 0
    $null = [NslLoaderSmokeWindow]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    if ($owner -ne $process.Id) { throw 'The renderer window belongs to a different process' }
    $rect = New-Object NslLoaderSmokeWindow+RECT
    if (-not [NslLoaderSmokeWindow]::GetWindowRect($hwnd, [ref]$rect)) { throw 'Renderer window geometry unavailable' }
    $width = [Math]::Min(1920, $rect.Right - $rect.Left)
    $height = [Math]::Min(1080, $rect.Bottom - $rect.Top)
    if ($width -lt 320 -or $height -lt 240) { throw 'Renderer window is too small for loading verification' }
    $image = New-Object Drawing.Bitmap($width, $height)
    try {
        $graphics = [Drawing.Graphics]::FromImage($image)
        try {
            $dc = $graphics.GetHdc()
            try { $captured = [NslLoaderSmokeWindow]::PrintWindow($hwnd, $dc, [uint32]2) }
            finally { $graphics.ReleaseHdc($dc) }
        } finally { $graphics.Dispose() }
        if (-not $captured) { throw 'Unable to capture the owned GPU window' }
        $colors = New-Object 'System.Collections.Generic.HashSet[int]'
        foreach ($y in 0..15) {
            foreach ($x in 0..23) {
                $px = [Math]::Min($width - 1, [int](($x + 0.5) * $width / 24))
                $py = [Math]::Min($height - 1, [int](($y + 0.5) * $height / 16))
                $null = $colors.Add($image.GetPixel($px, $py).ToArgb())
            }
        }
        if ($colors.Count -lt 20) { throw 'Renderer capture is blank or not yet visibly loaded' }
        $image.Save($capture, [Drawing.Imaging.ImageFormat]::Png)
    } finally { $image.Dispose() }

    if (-not (Test-Path -LiteralPath $log)) { throw 'Eden did not produce a loader log' }
    $engine = Get-Content -LiteralPath $log -Raw -Encoding UTF8
    $loaderPresent = $engine -match '(?i)Loader|NCA|ExeFS'
    $gpuPresent = $engine -match '(?i)Renderer|Vulkan|VideoCore|GPU'
    $badHeader = $engine -match '(?i)NCA bad header|0007-000C|Error 12'
    $critical = @($engine -split '\r?\n' | Where-Object { $_ -match '(?i)<Critical>|Fatal' }).Count
    $hashPreserved = (Get-FileHash -LiteralPath $gamePath -Algorithm SHA256).Hash -eq $hashBefore
    if (-not ($loaderPresent -and $gpuPresent -and (-not $badHeader) -and $critical -eq 0 -and $hashPreserved)) {
        throw 'Eden did not meet the isolated loading-only acceptance criteria'
    }
    $report = [ordered]@{
        status = 'NSL_EDEN_LOADER_ONLY_PASS'; observed_seconds = $ObserveSeconds
        source_sha256_unchanged = $hashPreserved; window_width = $width; window_height = $height
        sampled_unique_colors = $colors.Count; loader_log_present = $loaderPresent
        gpu_renderer_seen = $gpuPresent; bad_nca_header = $badHeader; critical_errors = $critical
        menu_interaction_attempted = $false; other_emulators_untouched = $true
    }
    $report | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath $reportPath -Encoding UTF8
    Write-Host ('NSL_EDEN_LOADER_ONLY_PASS seconds=' + $ObserveSeconds + ' colors=' + $colors.Count + ' window=' + $width + 'x' + $height)
} catch {
    # Keep diagnostic engine output if the loader fails, before destroying
    # the private profile and sensitive key copies. Never log any key values.
    $failureId = [Guid]::NewGuid().ToString('N').Substring(0, 8)
    foreach ($item in @(
        @{ Source = $log; Name = 'engine' },
        @{ Source = (Join-Path $private 'stdout.log'); Name = 'stdout' },
        @{ Source = (Join-Path $private 'stderr.log'); Name = 'stderr' }
    )) {
        if (Test-Path -LiteralPath $item.Source) {
            $diagnostic = Join-Path $qa ('eden_loader_failure_' + $failureId + '_' + $item.Name + '.log')
            Copy-Item -LiteralPath $item.Source -Destination $diagnostic -ErrorAction SilentlyContinue
        }
    }
    throw
} finally {
    $env:APPDATA = $originalAppdata
    $env:LOCALAPPDATA = $originalLocalappdata
    if ($null -ne $process) {
        $process.Refresh()
        if (-not $process.HasExited) {
            $owned = Get-CimInstance Win32_Process -Filter ('ProcessId=' + $process.Id) -ErrorAction SilentlyContinue
            if (-not $owned -or -not $owned.ExecutablePath.Equals($exe, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Refusing to terminate a process that does not match the isolated Eden executable'
            }
            Stop-Process -Id $process.Id -Force -ErrorAction Stop
            $null = $process.WaitForExit(5000)
        }
    }
    # Delete private copies of title/prod keys before retrying the executable
    # cleanup. Windows may keep the terminated .exe locked briefly while it
    # finishes process teardown or antivirus inspection.
    $privateKeys = Join-Path $private 'user\keys'
    if (Test-Path -LiteralPath $privateKeys) {
        Remove-Item -LiteralPath $privateKeys -Recurse -Force -ErrorAction Stop
    }
    for ($attempt = 1; $attempt -le 12; $attempt++) {
        try {
            if (Test-Path -LiteralPath $private) {
                Remove-Item -LiteralPath $private -Recurse -Force -ErrorAction Stop
            }
            break
        } catch {
            if ($attempt -eq 12) { throw }
            Start-Sleep -Milliseconds 600
        }
    }
}
