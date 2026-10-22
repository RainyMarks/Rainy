# Drive one window of a process by posting messages to it; nothing goes through the global mouse or keyboard,
# so other windows on the desktop never receive input. Steps are separated by ';;':
#   click:X,Y        left click at window-relative physical pixels (rclick: right click)
#   wheel:X,Y,N      turn the mouse wheel N notches at X,Y (negative scrolls down)
#   type:TEXT        type text (Unicode, as WM_CHAR)
#   typefile:PATH    type the contents of a UTF-8 file
#   hittest:X,Y      print WM_NCHITTEST's answer at X,Y after moving the mouse there (1 client, 2 caption)
#   busy:            print how long the window's thread stays busy (after a click, say)
#   key:NAME         press enter, escape, tab, down, up, backspace; prefix ctrl+ or shift+
#   wait:SECONDS     sleep
#   shot:PATH        capture the window to PATH
#   powershell -File native/scripts/drive.ps1 -ProcessId <pid> -Steps "click:900,1260;;type:hello;;key:enter;;wait:3;;shot:a.png"
param([Parameter(Mandatory = $true)][int]$ProcessId, [Parameter(Mandatory = $true)][string]$Steps)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Drive {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll", EntryPoint = "PostMessageW")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern uint MapVirtualKey(uint code, uint type);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
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
  static IntPtr KeyParam(uint vk, bool up) {
    uint scan = MapVirtualKey(vk, 0);
    uint value = 1 | (scan << 16);
    if (up) value |= (1u << 30) | (1u << 31);
    return (IntPtr)(long)value;
  }
  public static void Key(IntPtr h, uint vk) {
    PostMessage(h, 0x0100, (IntPtr)vk, KeyParam(vk, false));
    PostMessage(h, 0x0101, (IntPtr)vk, KeyParam(vk, true));
  }
  public static void Down(IntPtr h, uint vk) { PostMessage(h, 0x0100, (IntPtr)vk, KeyParam(vk, false)); }
  public static void Up(IntPtr h, uint vk) { PostMessage(h, 0x0101, (IntPtr)vk, KeyParam(vk, true)); }
  public static void Type(IntPtr h, string text) {
    foreach (char c in text) { PostMessage(h, 0x0102, (IntPtr)c, (IntPtr)1); System.Threading.Thread.Sleep(4); }
  }
  public static void Wheel(IntPtr h, int screenX, int screenY, int notches) {
    POINT p; p.X = screenX; p.Y = screenY; ScreenToClient(h, ref p);
    PostMessage(h, 0x0200, IntPtr.Zero, (IntPtr)((p.Y << 16) | (p.X & 0xFFFF)));
    IntPtr wparam = (IntPtr)((notches * 120) << 16);
    PostMessage(h, 0x020A, wparam, (IntPtr)((screenY << 16) | (screenX & 0xFFFF)));
  }
  [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr h, uint m, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
  /// How long the window's thread stays busy: until it has answered WM_NULL within 20 ms for 600 ms in a row.
  public static string Busy(IntPtr h) {
    var watch = System.Diagnostics.Stopwatch.StartNew(); double longest = 0, last = 0; IntPtr r;
    while (watch.ElapsedMilliseconds < 15000) {
      var t = watch.Elapsed.TotalMilliseconds;
      SendMessageTimeout(h, 0, IntPtr.Zero, IntPtr.Zero, 0, 20000, out r);
      var d = watch.Elapsed.TotalMilliseconds - t;
      if (d > 20) { last = watch.Elapsed.TotalMilliseconds; if (d > longest) longest = d; }
      if (watch.Elapsed.TotalMilliseconds - last > 600) break;
      System.Threading.Thread.Sleep(2);
    }
    return String.Format("busy until {0:N0} ms, longest stall {1:N0} ms", last, longest);
  }
  [DllImport("user32.dll", EntryPoint = "SendMessageW")] public static extern IntPtr Send(IntPtr h, uint msg, IntPtr w, IntPtr l);
  /// What the window answers to WM_NCHITTEST at a screen point after the mouse moved there (1 client, 2 caption).
  public static long HitTest(IntPtr h, int screenX, int screenY) {
    POINT p; p.X = screenX; p.Y = screenY; ScreenToClient(h, ref p);
    PostMessage(h, 0x0200, IntPtr.Zero, (IntPtr)((p.Y << 16) | (p.X & 0xFFFF)));
    System.Threading.Thread.Sleep(300);
    return (long)Send(h, 0x0084, IntPtr.Zero, (IntPtr)((screenY << 16) | (screenX & 0xFFFF)));
  }
  public static void Click(IntPtr h, int screenX, int screenY, bool right) {
    POINT p; p.X = screenX; p.Y = screenY; ScreenToClient(h, ref p);
    IntPtr pos = (IntPtr)((p.Y << 16) | (p.X & 0xFFFF));
    PostMessage(h, 0x0200, IntPtr.Zero, pos);
    if (right) { PostMessage(h, 0x0204, (IntPtr)2, pos); PostMessage(h, 0x0205, IntPtr.Zero, pos); }
    else { PostMessage(h, 0x0201, (IntPtr)1, pos); PostMessage(h, 0x0202, IntPtr.Zero, pos); }
  }
}
"@
[Drive]::SetProcessDPIAware() | Out-Null
$h = [Drive]::Largest([uint32]$ProcessId)
if ($h -eq [IntPtr]::Zero) { Write-Output "no window"; exit 1 }
if ([Drive]::IsIconic($h)) { [Drive]::ShowWindow($h, 9) | Out-Null; Start-Sleep -Milliseconds 800 }
$vks = @{ enter = 0x0D; escape = 0x1B; tab = 0x09; down = 0x28; up = 0x26; backspace = 0x08 }
$mods = @{ ctrl = 0x11; shift = 0x10; alt = 0x12 }
foreach ($step in ($Steps -split ';;')) {
  $kind, $arg = $step -split ':', 2
  $r = New-Object Drive+RECT; [Drive]::GetWindowRect($h, [ref]$r) | Out-Null
  switch ($kind) {
    'click' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $false); Start-Sleep -Milliseconds 200 }
    'wheel' { $x, $y, $n = $arg -split ','; [Drive]::Wheel($h, $r.Left + [int]$x, $r.Top + [int]$y, [int]$n); Start-Sleep -Milliseconds 150 }
    'rclick' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $true); Start-Sleep -Milliseconds 200 }
    'type' { [Drive]::Type($h, $arg); Start-Sleep -Milliseconds 150 }
    'clickbusy' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $false); Write-Output ("click " + $arg + ": " + [Drive]::Busy($h)) }
    'hittest' { $x, $y = $arg -split ','; Write-Output ("hittest " + $arg + ": " + [Drive]::HitTest($h, $r.Left + [int]$x, $r.Top + [int]$y)) }
    'busy' { Write-Output ("busy: " + [Drive]::Busy($h)) }
    'typefile' { [Drive]::Type($h, [IO.File]::ReadAllText($arg).TrimEnd()); Start-Sleep -Milliseconds 150 }
    'key' {
      $parts = $arg -split '\+'
      if ($parts.Count -gt 1) { [Drive]::Down($h, $mods[$parts[0]]) }
      $name = $parts[-1]
      $vk = if ($vks.ContainsKey($name)) { $vks[$name] } else { [int][char]$name.ToUpper() }
      [Drive]::Key($h, $vk)
      if ($parts.Count -gt 1) { [Drive]::Up($h, $mods[$parts[0]]) }
      Start-Sleep -Milliseconds 150
    }
    'wait' { Start-Sleep -Milliseconds ([int]([double]$arg * 1000)) }
    'shot' {
      $w = $r.Right - $r.Left; $hgt = $r.Bottom - $r.Top
      $bmp = New-Object System.Drawing.Bitmap $w, $hgt
      $g = [System.Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
      [Drive]::PrintWindow($h, $hdc, 2) | Out-Null
      $g.ReleaseHdc($hdc); $bmp.Save($arg, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
      Write-Output "shot $arg"
    }
  }
}
