# ui-click.ps1 — 在全局物理坐标处 SendInput 左键单击。
# 坐标换算：先 list_displays 确认窗口所在屏；display N 的屏内坐标 = 全局坐标 - 屏原点。
# 截图 raster → 物理：raster × (屏宽 / raster 宽)（通常 1.5，即 1280x720 raster ↔ 1920x1080 屏）。
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File ui-click.ps1 -x 2851 -y 475 [-dy 500]
param(
  [int]$x,
  [int]$y,
  [int]$notches = 0   # >0 时先点击聚焦，再在该点滚轮 $notches 格（负值向上）
)
$sig = @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
[DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, System.UIntPtr dwExtraInfo);
'@
Add-Type -MemberDefinition $sig -Name WinC -Namespace NativeClick | Out-Null
[NativeClick.WinC]::SetCursorPos($x, $y) | Out-Null
Start-Sleep -Milliseconds 120
[NativeClick.WinC]::mouse_event(0x0002, 0, 0, 0, [System.UIntPtr]::Zero)  # LEFTDOWN
Start-Sleep -Milliseconds 60
[NativeClick.WinC]::mouse_event(0x0004, 0, 0, 0, [System.UIntPtr]::Zero)  # LEFTUP
if ($notches -ne 0) {
  Start-Sleep -Milliseconds 150
  $dir = [uint32]::MaxValue - 119   # WHEEL_DELTA=120 向下；负数向上用 +120
  for ($i = 0; $i -lt [Math]::Abs($notches); $i++) {
    if ($notches -gt 0) { [NativeClick.WinC]::mouse_event(0x0800, 0, 0, $dir, [System.UIntPtr]::Zero) }
    else { [NativeClick.WinC]::mouse_event(0x0800, 0, 0, 120, [System.UIntPtr]::Zero) }
    Start-Sleep -Milliseconds 40
  }
}
Write-Output "CLICK $x $y (notches=$notches)"
