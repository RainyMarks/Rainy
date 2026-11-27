# Drive one window of a process by posting messages to it; nothing goes through the global mouse or keyboard,
# so other windows on the desktop never receive input. Steps are separated by ';;':
#   click:X,Y        left click at window-relative physical pixels (rclick: right click)
#   drag:X1,Y1,X2,Y2 press the left button at X1,Y1, move to X2,Y2 with it held and release there
#   wheel:X,Y,N      turn the mouse wheel N notches at X,Y (negative scrolls down)
#   type:TEXT        type text (Unicode, as WM_CHAR)
#   typefile:PATH    type the contents of a UTF-8 file
#   hittest:X,Y      print WM_NCHITTEST's answer at X,Y after moving the mouse there (1 client, 2 caption)
#   busy:            print how long the window's thread stays busy (after a click, say)
#   key:NAME         press enter, escape, tab, down, up, backspace; prefix ctrl+ or shift+
#   chord:ctrl+K     press a key with ctrl, shift and/or alt held in the window's own key state (chord:ctrl+shift+A)
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
  [DllImport("user32.dll")] static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] static extern bool GetKeyboardState(byte[] state);
  [DllImport("user32.dll")] static extern bool SetKeyboardState(byte[] state);
  [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
  /// A key with a modifier held, as the window's own thread sees it: the modifier is set in that thread's key state
  /// (shared for a moment through AttachThreadInput), never in the system's, so no other window gets it.
  public static void Chord(IntPtr h, uint[] modifiers, uint vk) {
    uint pid; uint target = GetWindowThreadProcessId(h, out pid);
    uint self = GetCurrentThreadId();
    AttachThreadInput(self, target, true);
    var state = new byte[256];
    GetKeyboardState(state);
    var before = (byte[])state.Clone();
    foreach (uint modifier in modifiers) { state[modifier] = 0x80; }
    SetKeyboardState(state);
    PostMessage(h, 0x0100, (IntPtr)vk, KeyParam(vk, false));
    PostMessage(h, 0x0101, (IntPtr)vk, KeyParam(vk, true));
    System.Threading.Thread.Sleep(300);
    SetKeyboardState(before);
    AttachThreadInput(self, target, false);
  }
  public static void Click(IntPtr h, int screenX, int screenY, bool right) {
    POINT p; p.X = screenX; p.Y = screenY; ScreenToClient(h, ref p);
    IntPtr pos = (IntPtr)((p.Y << 16) | (p.X & 0xFFFF));
    PostMessage(h, 0x0200, IntPtr.Zero, pos);
    if (right) { PostMessage(h, 0x0204, (IntPtr)2, pos); PostMessage(h, 0x0205, IntPtr.Zero, pos); }
    else { PostMessage(h, 0x0201, (IntPtr)1, pos); PostMessage(h, 0x0202, IntPtr.Zero, pos); }
  }
  // Press the left button at the start, move in steps with it held (MK_LBUTTON), release at the end.
  public static void Drag(IntPtr h, int x1, int y1, int x2, int y2) {
    const int steps = 8;
    for (int i = 0; i <= steps; i++) {
      POINT p; p.X = x1 + (x2 - x1) * i / steps; p.Y = y1 + (y2 - y1) * i / steps; ScreenToClient(h, ref p);
      IntPtr pos = (IntPtr)((p.Y << 16) | (p.X & 0xFFFF));
      if (i == 0) { PostMessage(h, 0x0200, IntPtr.Zero, pos); PostMessage(h, 0x0201, (IntPtr)1, pos); }
      else PostMessage(h, 0x0200, (IntPtr)1, pos);
      System.Threading.Thread.Sleep(30);
      if (i == steps) PostMessage(h, 0x0202, IntPtr.Zero, pos);
    }
  }
}
"@
[Drive]::SetProcessDPIAware() | Out-Null
$h = [Drive]::Largest([uint32]$ProcessId)
if ($h -eq [IntPtr]::Zero) { Write-Output "no window"; exit 1 }
# Restored without taking the focus from whatever the user is doing.
if ([Drive]::IsIconic($h)) { [Drive]::ShowWindow($h, 4) | Out-Null; Start-Sleep -Milliseconds 800 }
$vks = @{
  enter = 0x0D; escape = 0x1B; tab = 0x09; down = 0x28; up = 0x26; left = 0x25; right = 0x27;
  backspace = 0x08; delete = 0x2E; insert = 0x2D; home = 0x24; end = 0x23; pgup = 0x21; pgdn = 0x22;
  space = 0x20;
  f1 = 0x70; f2 = 0x71; f3 = 0x72; f4 = 0x73; f5 = 0x74; f6 = 0x75;
  f7 = 0x76; f8 = 0x77; f9 = 0x78; f10 = 0x79; f11 = 0x7A; f12 = 0x7B;
  '/' = 0xBF; '?' = 0xBF; '-' = 0xBD; '_' = 0xBD; '=' = 0xBB; '+' = 0xBB;
  '[' = 0xDB; ']' = 0xDD; '\' = 0xDC; ';' = 0xBA; "'" = 0xDE; ',' = 0xBC; '.' = 0xBE; '`' = 0xC0;
  slash = 0xBF; minus = 0xBD; equal = 0xBB; comma = 0xBC; period = 0xBE;
}
$mods = @{ ctrl = 0x11; shift = 0x10; alt = 0x12 }
function Resolve-Vk([string]$name) {
  $key = $name.ToLowerInvariant()
  if ($vks.ContainsKey($key)) { return [uint32]$vks[$key] }
  if ($name.Length -eq 1 -and $name -match '^[A-Za-z0-9]$') { return [uint32][int][char]$name.ToUpperInvariant() }
  throw "Unknown key name: $name"
}
function Resolve-Modifier([string]$name) {
  $key = $name.ToLowerInvariant()
  if (-not $mods.ContainsKey($key)) { throw "Unknown modifier: $name" }
  return [uint32]$mods[$key]
}
function Resolve-LocalPath([string]$raw) {
  if ([IO.Path]::IsPathRooted($raw)) { return $raw }
  return [IO.Path]::GetFullPath((Join-Path $PWD.Path $raw))
}
foreach ($step in ($Steps -split ';;')) {
  if ([string]::IsNullOrWhiteSpace($step)) { continue }
  $kind, $arg = $step -split ':', 2
  $r = New-Object Drive+RECT; [Drive]::GetWindowRect($h, [ref]$r) | Out-Null
  switch ($kind) {
    'click' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $false); Start-Sleep -Milliseconds 200 }
    'drag' { $x1, $y1, $x2, $y2 = $arg -split ','; [Drive]::Drag($h, $r.Left + [int]$x1, $r.Top + [int]$y1, $r.Left + [int]$x2, $r.Top + [int]$y2); Start-Sleep -Milliseconds 200 }
    'wheel' { $x, $y, $n = $arg -split ','; [Drive]::Wheel($h, $r.Left + [int]$x, $r.Top + [int]$y, [int]$n); Start-Sleep -Milliseconds 150 }
    'rclick' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $true); Start-Sleep -Milliseconds 200 }
    'type' { [Drive]::Type($h, $arg); Start-Sleep -Milliseconds 150 }
    'clickbusy' { $x, $y = $arg -split ','; [Drive]::Click($h, $r.Left + [int]$x, $r.Top + [int]$y, $false); Write-Output ("click " + $arg + ": " + [Drive]::Busy($h)) }
    'hittest' { $x, $y = $arg -split ','; Write-Output ("hittest " + $arg + ": " + [Drive]::HitTest($h, $r.Left + [int]$x, $r.Top + [int]$y)) }
    'busy' { Write-Output ("busy: " + [Drive]::Busy($h)) }
    'typefile' { [Drive]::Type($h, [IO.File]::ReadAllText((Resolve-LocalPath $arg)).TrimEnd()); Start-Sleep -Milliseconds 150 }
    'chord' {
      $parts = $arg -split '\+'
      $vk = Resolve-Vk $parts[-1]
      [uint32[]]$held = if ($parts.Count -gt 1) { @($parts[0..($parts.Count - 2)] | ForEach-Object { Resolve-Modifier $_ }) } else { @() }
      [Drive]::Chord($h, $held, $vk)
      Start-Sleep -Milliseconds 150
    }
    'key' {
      $parts = $arg -split '\+'
      [uint32[]]$held = if ($parts.Count -gt 1) { @($parts[0..($parts.Count - 2)] | ForEach-Object { Resolve-Modifier $_ }) } else { @() }
      foreach ($mod in $held) { [Drive]::Down($h, $mod) }
      [Drive]::Key($h, (Resolve-Vk $parts[-1]))
      for ($i = $held.Count - 1; $i -ge 0; $i--) { [Drive]::Up($h, $held[$i]) }
      Start-Sleep -Milliseconds 150
    }
    'wait' { Start-Sleep -Milliseconds ([int]([double]$arg * 1000)) }
    'shot' {
      $w = $r.Right - $r.Left; $hgt = $r.Bottom - $r.Top
      if ($w -le 0 -or $hgt -le 0) { Write-Error "Invalid window dimensions: ${w}x${hgt}"; exit 1 }
      $outPath = Resolve-LocalPath $arg
      $parent = Split-Path -Parent $outPath
      if ($parent -and -not (Test-Path -LiteralPath $parent)) { [void][IO.Directory]::CreateDirectory($parent) }
      $bmp = New-Object System.Drawing.Bitmap $w, $hgt
      $g = [System.Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
      [Drive]::PrintWindow($h, $hdc, 2) | Out-Null
      $g.ReleaseHdc($hdc); $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
      Write-Output "shot $arg"
    }
    default {
      Write-Error "Unknown drive step: $kind"
      exit 1
    }
  }
}
