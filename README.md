# NightDisplay — 每日定时黑屏（Windows / macOS 图形界面版）

给需要“晚上屏幕黑着、但电脑继续开着、可以用 UU 远程操作”的场景用。可见、可恢复、需主动启用。
本工具**不是** UU 远程官方插件，不拦截键盘鼠标，不安装隐藏服务，不联网，不需要管理员权限。

## 下载
到 [Releases](../../releases) 下载：
- `NightDisplay-Windows-x64.zip`：Windows 10/11（ARM 版 Windows 通过系统 x64 仿真运行）
- `NightDisplay-macOS-arm64.zip`：Apple 芯片 Mac（M1/M2/M3/M4…）
- `NightDisplay-macOS-x64.zip`：Intel 芯片 Mac

## 使用步骤
1. Windows：解压后把 `NightDisplay.exe` 放到固定文件夹（不要放在“下载”里随手删）。
   macOS：把 `NightDisplay.app` 拖到“应用程序”。首次打开若提示“无法验证开发者”，右键 → 打开；或执行 `xattr -dr com.apple.quarantine /Applications/NightDisplay.app`。
2. 打开程序，选择**开始黑屏时间**和**恢复显示时间**（可跨午夜，如 20:00 → 10:30）。
3. 点 **测试（120秒自动恢复）**：立刻黑屏，120 秒后自动恢复。此时用 UU 远程连接检查是否能操作。
4. 确认恢复正常后点 **启用每日计划**（24 小时内必须测试过同一模式，否则拒绝启用）。
5. 不想用了：**暂停计划**；出问题：**应急恢复**。

## 模式说明
| 模式 | 平台 | 做法 | 限制 |
|---|---|---|---|
| 基础关屏 ScreenOff | Win / Mac | Win: 发送显示器关闭消息；Mac: `pmset displaysleepnow` | 本地动鼠标/键盘就会亮屏；部分远程软件远程看到的也是黑屏 |
| 仅虚拟屏 VirtualOnly | 仅 Windows | `DisplaySwitch /extend` 再 `/external`，只保留第二屏（虚拟屏） | 需先自行安装虚拟显示驱动 [VDD](https://github.com/VirtualDrivers/Virtual-Display-Driver)，且虚拟屏必须是“第二显示器”；实体屏背光是否全黑看硬件 |

## 计划任务（全部可见）
- Windows：任务计划程序中的 `NightDisplay_Night`、`NightDisplay_Day`（在设定时间运行 `NightDisplay.exe --action reconcile`），以及 `HKCU\...\Run\NightDisplay` 登录时校准（可在任务管理器“启动”页看到）。
- macOS：`~/Library/LaunchAgents/com.nightdisplay.reconcile.plist`（设定时间 + 登录时运行）。黑屏期间用 `caffeinate -i -s` 阻止系统睡眠，到恢复时间停止。
- 移动了程序位置后需要重新点“启用每日计划”。

## macOS 的诚实说明
- macOS 没有 Windows 那样的“只保留虚拟屏”命令。若希望本地屏黑而远程有画面，可用第三方工具如 **BetterDisplay**（可创建虚拟屏并关闭/镜像内建屏）或 HDMI 假负载（显卡欺骗器），本程序不集成它们。
- 笔记本合盖会睡眠，除非接电源+外接显示器；请在“系统设置 → 电池/节能”里设置接电源时不自动睡眠。
- UU 远程等需要“屏幕录制”和“辅助功能”权限；显示器睡眠后部分远程软件可能只看到黑屏，需要实测。

## 应急恢复
- 程序内点“应急恢复”，或命令行：`NightDisplay.exe --action emergency`（Mac：`/Applications/NightDisplay.app/Contents/MacOS/NightDisplay --action emergency`）。
- Windows 仍黑屏：按 `Win+P` 选“仅电脑屏幕”；再不行进安全模式禁用虚拟显示驱动。

## 命令行
`--action gui|night|day|reconcile|emergency|pause|status [--mode ScreenOff|VirtualOnly]`

## 数据位置
Windows `%LOCALAPPDATA%\NightDisplay`；macOS `~/Library/Application Support/NightDisplay`（config.json、nightdisplay.log）。

## 测试状态
CI 中验证：时间段逻辑单元测试、打包后程序可启动并输出状态。**未在真实一体机、UU 远程和虚拟显示驱动上实测**，请先用测试功能。
`legacy/` 为原 v0.1 PowerShell 版本。
