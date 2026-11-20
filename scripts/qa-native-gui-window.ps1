param(
    [string]$Executable = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\nsl.exe'),
    [string]$ExpectedCommit = ''
)

$ErrorActionPreference = 'Stop'

# Target only a newly launched, isolated NSL process. Do not use global mouse
# or keyboard input, foreground activation, or the user's application data.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NslWindowProbe {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool SetWindowPos(IntPtr hwnd, IntPtr behind, int x, int y,
                                            int width, int height, uint flags);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")]
    public static extern bool IsHungAppWindow(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message,
        IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out IntPtr result);
}
'@

$source = (Resolve-Path -LiteralPath $Executable).Path
$temp = Join-Path ([IO.Path]::GetTempPath()) ('NSL_GUI_NATIVE_' + [guid]::NewGuid().ToString('N'))
$process = $null
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $copy = Join-Path $temp 'nsl.exe'
    Copy-Item -LiteralPath $source -Destination $copy
    $process = Start-Process -FilePath $copy -WorkingDirectory $temp -WindowStyle Minimized -PassThru
    $handle = [IntPtr]::Zero
    $title = ''
    for ($i = 0; $i -lt 50; $i++) {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
        if ($process.HasExited) { throw "Isolated GUI quit early: $($process.ExitCode)" }
        $handle = $process.MainWindowHandle
        $title = $process.MainWindowTitle
        if ($handle -ne [IntPtr]::Zero -and $title) { break }
    }
    if ($handle -eq [IntPtr]::Zero -or $title -notmatch '^NSL\s+') {
        throw "Expected NSL main window, got: '$title'"
    }
    if ($ExpectedCommit -and -not $title.Contains($ExpectedCommit.Substring(0, [Math]::Min(7, $ExpectedCommit.Length)))) {
        throw "Main window did not identify build ${ExpectedCommit}: $title"
    }
    Write-Output "GUI_NATIVE_TITLE=$title"
    if (-not (Test-Path -LiteralPath (Join-Path $temp 'library.db'))) {
        throw 'Portable GUI did not create its isolated database'
    }

    # SW_SHOWNOACTIVATE=4; SWP_NOACTIVATE|SWP_NOZORDER=0x14.
    # Reposition only our own test window, without stealing keyboard focus.
    [void][NslWindowProbe]::ShowWindow($handle, 4)
    foreach ($size in @(@(980, 720), @(1280, 800), @(1456, 900))) {
        if (-not [NslWindowProbe]::SetWindowPos($handle, [IntPtr]::Zero,
            30, 30, $size[0], $size[1], 0x14)) {
            throw "Could not resize the isolated window to $($size[0])x$($size[1])"
        }
        Start-Sleep -Milliseconds 220
        $rect = New-Object NslWindowProbe+Rect
        if (-not [NslWindowProbe]::GetWindowRect($handle, [ref]$rect)) {
            throw 'Could not inspect the isolated window size'
        }
        $width = $rect.Right - $rect.Left
        $height = $rect.Bottom - $rect.Top
        if ([Math]::Abs($width - $size[0]) -gt 2 -or [Math]::Abs($height - $size[1]) -gt 2) {
            throw "Window geometry mismatch: $width x $height"
        }
        $response = [IntPtr]::Zero
        $ok = [NslWindowProbe]::SendMessageTimeout($handle, 0, [IntPtr]::Zero,
            [IntPtr]::Zero, 2, 3000, [ref]$response)
        if ($ok -eq [IntPtr]::Zero -or [NslWindowProbe]::IsHungAppWindow($handle)) {
            throw "Window failed to respond at $width x $height"
        }
        $process.Refresh()
        if ($process.HasExited) { throw 'NSL crashed while changing window size' }
        Write-Output "GUI_NATIVE_RESIZE_PASS=${width}x${height}"
    }
    Write-Output 'GUI_NATIVE_WINDOW_SMOKE=PASS'
}
finally {
    if ($null -ne $process) {
        try {
            $process.Refresh()
            if (-not $process.HasExited) {
                [void]$process.CloseMainWindow()
                if (-not $process.WaitForExit(5000)) {
                    $process.Kill()
                    [void]$process.WaitForExit(5000)
                }
            }
        }
        catch { Write-Output "GUI_NATIVE_CLEANUP_WARNING=$($_.Exception.Message)" }
    }
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $temp) { throw 'Isolated GUI temporary folder was not removed' }
    Write-Output 'GUI_NATIVE_TEMP_CLEAN=YES'
}
