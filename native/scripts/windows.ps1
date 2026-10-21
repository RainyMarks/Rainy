# List every top-level window of a process: handle, visibility, size, class and title.
param([Parameter(Mandatory = $true)][int]$ProcessId)
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  public static void List(uint pid) {
    EnumWindows((h, p) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != pid) return true;
      RECT r; GetWindowRect(h, out r);
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      var t = new StringBuilder(256); GetWindowText(h, t, 256);
      Console.WriteLine("{0} visible={1} iconic={2} rect=({3},{4})-({5},{6}) class={7} title={8}", h, IsWindowVisible(h), IsIconic(h), r.Left, r.Top, r.Right, r.Bottom, c, t);
      return true;
    }, IntPtr.Zero);
  }
}
"@
[W]::SetProcessDPIAware() | Out-Null
[W]::List([uint32]$ProcessId)
$p = Get-Process -Id $ProcessId; "responding=$($p.Responding) cpu=$($p.CPU) threads=$($p.Threads.Count)"
