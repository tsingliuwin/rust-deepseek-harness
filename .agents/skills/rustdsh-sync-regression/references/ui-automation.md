# UI 实机自动化（GPUI 应用的点击/截图回归）

rustdsh 是 GPUI 自绘 UI，**无控件级 a11y 树**，CUA 的 a11y 优先路径拿不到内部元素；
且 CUA 坐标点击的帧绑定在本应用上持续报 identity 错误。已验证的可行路径：
**PowerShell SendInput 物理点击（脚本）+ CUA screenshot 仅作观察**。

## 前置

1. **构建并重启宿主**：`bash scripts/restart-host.sh`（含流式静默检查，勿让用户手动重启）。
2. **静默桌面**：`powershell -NoProfile -ExecutionPolicy Bypass -File .agents/skills/rustdsh-sync-regression/scripts/ui-quiet.ps1`
   —— 最小化所有其它顶层窗口 + TOPMOST 置顶 dsh-gpui。否则全屏截图帧会被动态内容（视频、闪烁光标）秒杀（stale）。
3. **确认窗口所在屏**：`list_displays` + CUA 截图核对。**本机双屏**：display 2 的屏内物理坐标要 +1920（屏原点 1920,0）才是全局坐标。

## 坐标换算

```
全局坐标 = 屏原点 + raster坐标 × (屏宽 / raster宽)
```
- 通常 raster 1280x720 ↔ 屏幕 1920x1080，比例 1.5。
- display 1：全局 = raster × 1.5；display 2：全局 = (raster.x × 1.5 + 1920, raster.y × 1.5)。
- **每次点击前先截屏确认目标位置**（滚动/弹层会移动元素）。

## 点击与滚动

```powershell
# 单击（全局坐标）
powershell -NoProfile -ExecutionPolicy Bypass -File .agents/skills/rustdsh-sync-regression/scripts/ui-click.ps1 -x 2851 -y 475
# 点击聚焦 + 向下滚 5 格（设置面板内容区）
powershell -NoProfile -ExecutionPolicy Bypass -File .agents/skills/rustdsh-sync-regression/scripts/ui-click.ps1 -x 1450 -y 700 -notches 5
```

## 文本输入

GPUI 的 Input **吃 CUA 合成按键不可靠**（Delete/ctrl+a 常无效），已验证的组合：
1. 物理点击聚焦输入框（ui-click.ps1）。
2. `type(app_ref={"pid": <pid>}, text="...")` 输入文本（CUA app 级键入，前台应用有效）。
3. 改写内容：重新聚焦后连发 BackSpace（repeat 参数）逐字删除，再输入新文本。

## 截图观察

- `screenshot` 抓当前屏（先 `switch_display` 到窗口所在屏）。
- `zoom`（带 frame_id + region）放大按钮/文字核对状态。
- 每步操作后立即截屏判读；断言失败时回到最近一次好截图，缩小怀疑范围。

## 已知坑

- **TOPMOST 残留**：回归结束必须取消置顶（ui-quiet.ps1 结束时若不再继续操作，可手动
  `SetWindowPos(HWND_NOTOPMOST)`），否则用户屏幕上应用恒置顶。
- **误点风险**：窗口不在预期屏时点击会落到用户其它窗口上——点击前必须截图确认窗口位置。
- **mock 端点**：成功路径用本地 mock（python http.server 返回 `{data:[...]}`，端口自选），
  测完 `taskkill //F //IM python.exe` 清理；不要把用户真实密钥打进输入框。
- **应用日志**：GPUI 应用无控制台输出；排障靠单测断言、examples（dump_lines/inspect_kinds
  解 zstd 会话日志）、以及 UI 截图对照上游源码数值。
