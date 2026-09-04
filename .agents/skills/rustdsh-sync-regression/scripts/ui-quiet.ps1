# ui-quiet.ps1 — 静默桌面：最小化其它可见顶层窗口 + 恢复/TOPMOST 置顶 dsh-gpui。
# 背景：全屏截图帧会被动态内容（视频/闪烁光标）秒杀；GPUI 无 a11y 树，CUA 坐标
# 点击的帧绑定在本应用上不可靠，回归脚本改用 SendInput 物理点击 + CUA 截图观察。
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File ui-quiet.ps1
$sig = @'
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int X, int Y, int cx, int cy, uint flags);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc lpEnumFunc, IntPtr lParam);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder sb, int max);
public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
'@
Add-Type -MemberDefinition $sig -Name WinQ -Namespace NativeQuiet | Out-Null

$script:target = [IntPtr]::Zero
$others = New-Object System.Collections.ArrayList
$cb = [NativeQuiet+EnumProc]{ param($h, $l)
  if ([NativeQuiet.WinQ]::IsWindowVisible($h)) {
    $sb = New-Object System.Text.StringBuilder 256
    [NativeQuiet.WinQ]::GetWindowText($h, $sb, 256) | Out-Null
    $t = $sb.ToString()
    if ($t -eq 'DeepSeek Harness') { $script:target = $h }
    elseif ($t -ne '' -and $t -ne 'Program Manager') { [void]$others.Add($h) }
  }
  return $true
}
[NativeQuiet.WinQ]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null

if ($target -eq [IntPtr]::Zero) { Write-Output 'TARGET-NOT-FOUND'; exit 1 }
foreach ($h in $others) { [NativeQuiet.WinQ]::ShowWindow($h, 6) | Out-Null }   # SW_MINIMIZE
Start-Sleep -Milliseconds 400
[NativeQuiet.WinQ]::ShowWindow($target, 9) | Out-Null                          # SW_RESTORE
Start-Sleep -Milliseconds 200
[NativeQuiet.WinQ]::SetWindowPos($target, [IntPtr](-1), 0, 0, 0, 0, 0x3) | Out-Null  # TOPMOST
Start-Sleep -Milliseconds 200
Write-Output 'QUIET-OK (target topmost)'
