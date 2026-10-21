# Start a program, wait, capture its largest visible window to a PNG, then stop it.
#   powershell -File native/scripts/shot.ps1 -Exe <path> -Out <png> [-Wait 6] [-Keep] [-Args "..."]
# With -Attach <pid>, capture a running process instead of starting one (and never stop it).
param(
  [string]$Exe = "",
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$Wait = 6,
  [string]$Args = "",
  [int]$Attach = 0,
  [switch]$Keep
)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class Win {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  public static IntPtr Largest(uint pid) {
    IntPtr best = IntPtr.Zero; long area = 0;
    EnumWindows((h, p) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != pid || !IsWindowVisible(h)) return true;
      RECT r; GetWindowRect(h, out r);
      long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
      if (a > area) { area = a; best = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
}
"@
[Win]::SetProcessDPIAware() | Out-Null
if ($Attach -ne 0) { $p = Get-Process -Id $Attach; $Keep = $true }
elseif ($Args) { $p = Start-Process -FilePath $Exe -ArgumentList $Args -PassThru }
else { $p = Start-Process -FilePath $Exe -PassThru }
$deadline = (Get-Date).AddSeconds($Wait + 30)
$h = [IntPtr]::Zero
while ((Get-Date) -lt $deadline) {
  $p.Refresh()
  if ($p.HasExited) { Write-Output "exited with $($p.ExitCode)"; exit 1 }
  $h = [Win]::Largest([uint32]$p.Id)
  if ($h -ne [IntPtr]::Zero) {
    $r = New-Object Win+RECT; [Win]::GetWindowRect($h, [ref]$r) | Out-Null
    if ([Win]::IsIconic($h) -or ($r.Right - $r.Left) -gt 400) { break }
  }
  Start-Sleep -Milliseconds 250
}
Start-Sleep -Seconds $Wait
$p.Refresh()
if ($p.HasExited) { Write-Output "exited with $($p.ExitCode)"; exit 1 }
$h = [Win]::Largest([uint32]$p.Id)
# A window started minimized (the launcher's show state) is restored before it is captured.
if ([Win]::IsIconic($h)) { [Win]::ShowWindow($h, 9) | Out-Null; Start-Sleep -Milliseconds 800 }
$r = New-Object Win+RECT
[Win]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $hgt = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
# PW_RENDERFULLCONTENT captures DirectX content even when covered.
[Win]::PrintWindow($h, $hdc, 2) | Out-Null
$g.ReleaseHdc($hdc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "window ${w}x${hgt} pid $($p.Id) -> $Out"
if (-not $Keep) { Stop-Process -Id $p.Id -Force }
