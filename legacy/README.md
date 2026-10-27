# NightDisplay v0.1：Windows 一体机夜间显示管理（测试版）

这是一个**可见、可恢复、需主动启用**的 Windows 显示切换原型，不是 UU 远程的官方代码或插件，也不是 UU 防窥模式的克隆品。仅用于你拥有或已获授权管理的电脑。

## 可以做什么

- 每天 **20:00** 发出进入夜间显示模式的命令；每天 **10:30** 恢复白天显示。
- 可选择两条路线：`ScreenOff`（基础关屏）和 `VirtualOnly`（切换到第二显示屏，须预先安装虚拟显示驱动）。
- 通过 Windows 任务计划程序注册**可见**的每日任务，以及登录后的时间校准任务；重启后仍保留任务配置。
- 提供 120 秒自动恢复的手动测试、一键暂停、一键应急恢复、卸载任务和日志。
- **不会**关闭后台应用、安装远程控制服务、模拟键鼠防挂机，亦不会修改 UU 客户端。

## 无法保证什么

1. **基础关屏不是 UU 强隐私屏**：任意鼠标或键盘输入可能立即唤醒；并且某些远程软件会显示黑画面。
2. `VirtualOnly` 使用 Windows `DisplaySwitch.exe /external`，理论上只保留**第二显示器**。只有在第二显示器确认为虚拟显示器且 UU 能连接该虚拟屏时，才可能实现“本地屏不显示但远程可操作”。
3. Windows 不一定会切断所有一体机的实体屏幕背光；即使进入仅第二屏模式，能否做到像断电一样黑，仍取决于硬件驱动。
4. UU 可能会在连接/断开时自己切换显示器，导致冲突。必须实机验证；不支持就不要启用虚拟屏每日调度。
5. 本程序**不拦截本地键盘鼠标**，也不屏蔽管理员的正常控制。本地其他用户仍可能改变 Windows 显示设置。
6. **Windows 必须保持开机、登录用户会话存在，且不能睡眠**；任务配置的执行依赖该用户交互式登录会话。锁屏、注销、重启后的自动重新登录、远程会话恢复等需要根据实际环境测试，不能保证。
7. BOSS 直聘是否持续在线受网站行为、浏览器后台节流、网络和平台规则影响；此工具只负责显示切换，不保证挂机效果。

## 依赖

- Windows 10/11；系统自带 Windows PowerShell 5.1 和任务计划程序。
- 对应设备的合法管理/安装权限；正常情况下不需要 SYSTEM 权限。
- **强模式单独需要** Windows 虚拟显示驱动（本 ZIP **不包含**第三方驱动）：
  https://github.com/VirtualDrivers/Virtual-Display-Driver
- 远程操作需你自己已安装并授权配置好的 UU 远程；本程序不会安装或接入它。

## 使用步骤（一定按顺序）

1. 解压 ZIP 到**不会立即删除**的文件夹。右键选择 `Run.bat` → 打开。**不要默认选择以管理员身份运行**；任务绑定的是当前桌面用户。
2. 菜单 **1 安装**。脚本复制到 `%LOCALAPPDATA%\NightDisplay`，创建三个**初始暂停**的任务（20:00、10:30、登录校准）。不会安装完立刻黑屏。
3. 菜单 **2 检测硬件**，保存系统、显卡、显示器信息。建议同时在 Windows「设置 → 系统 → 屏幕」确认物理显示器和虚拟显示器的位置。
4. 菜单 **3 测试基础关屏**。会提前创建一个约 120 秒后的**独立恢复任务**。此时检查黑屏、鼠标唤醒与远程是否正常；到期屏幕应被唤醒。
5. 如果你希望接近 UU 的强防窥，先通过上面的 GitHub 项目自行安装签名虚拟显示驱动，然后在系统显示设置中将桌面**扩展**到虚拟显示器，并确认它被 Windows 识别为**第二显示器**。注意切回白天后虚拟显示器可以变为非活动状态，测试脚本会先执行 `/extend` 再执行 `/external`。远程连接 UU，确认它可以查看/控制这个虚拟屏，再用菜单 **4 测试虚拟屏独占**。测试前应具备**物理恢复途径**，强烈建议有人在电脑旁协助观察。
6. 菜单 4 测试成功后，等待至少 120 秒，确认 **PC 屏幕已恢复，UU 重新连接正常**。再选择菜单 **6 启用虚拟屏每日任务**，输入 `ENABLE`。基础模式成功则可以选菜单 5，但**基础模式不满足键鼠无法唤醒的要求**。
7. 还必须手动将一体机的 Windows 电源设置改为**接通电源时不自动睡眠**（关屏与系统睡眠是两件事）。
8. 进入菜单 **7 状态**核查：每日任务必须显示 Ready，启用模式符合预期；下一次执行时间正确。
9. 隔天检查事件日志 `%LOCALAPPDATA%\NightDisplay\nightdisplay.log`，并验证两次定时切换。

**特别注意**：`/external` 不会自动识别“哪块是虚拟显示器”。在多实体屏、一体机映射为外接屏、驱动列举顺序特殊的设备上，可能切错对象。此时不要启用每日任务。

## 紧急恢复

- 首选：双击 `Recover.bat`（恢复安装版的物理显示模式，同时暂停每日任务）。
- 或从 Windows PowerShell 执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\NightDisplay\NightDisplay.ps1" -Action Emergency
```

- `VirtualOnly` 的恢复动作是 `DisplaySwitch.exe /internal`，即 Windows 的“仅电脑屏幕”；**它在具体一体机上的结果必须提前验证**。
- 如实体屏无显示，使用 UU（若仍可用）发出恢复命令；或按 `Win+P` 切换显示模式，进入 Windows 安全模式卸载/禁用虚拟显示驱动。不要盲目安装多个显示驱动。

## 安装模式和手动命令

安装后的主脚本：`%LOCALAPPDATA%\NightDisplay\NightDisplay.ps1`。

```powershell
# 显示系统状态
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\NightDisplay\NightDisplay.ps1" -Action Status

# 关闭每天自动切换（不改变当前显示状态）
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\NightDisplay\NightDisplay.ps1" -Action Pause

# 手动恢复实体屏并暂停任务
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\NightDisplay\NightDisplay.ps1" -Action Emergency
```

按上述顺序在菜单中启用后，每天自动循环，**无需重新启动 BAT**。第二天 10:30 的任务是单独注册的，不是依赖通宵运行的 PowerShell 脚本。

## 计划任务和本地配置

- `NightDisplay_2000_Night`：每天 20:00。
- `NightDisplay_1030_Day`：每天 10:30。
- `NightDisplay_OnLogon`：登录后的昼夜模式校准。
- `NightDisplay_TestRecovery`：测试时创建的一次性恢复任务（测试后可能仍保留，可在应急操作中清理）。
- `config.json`：可见的当前模式、启用标记、测试记录。
- `nightdisplay.log`：运行日志。

脚本默认无管理员提权、无隐藏服务、无常驻后台守护进程、无输入拦截、无网络通信。

## 卸载

运行 `Run.bat` → 菜单 `U` → 输入 `REMOVE`。脚本尝试恢复实体屏，然后卸载所有 NightDisplay 定时任务。日志和配置留在 `%LOCALAPPDATA%\NightDisplay` 便于核对，关闭脚本窗口后可自行删除该文件夹。

**独立安装的 VDD 驱动不会自动卸载**，请按照驱动项目的说明从控制程序或设备管理器卸载。不要删除 Windows 系统目录中的驱动文件来代替卸载。

## 源码参考

- Microsoft `WM_SYSCOMMAND / SC_MONITORPOWER`: https://learn.microsoft.com/en-us/windows/win32/menurc/wm-syscommand
- Microsoft ScheduledTasks: https://learn.microsoft.com/en-us/powershell/module/scheduledtasks/register-scheduledtask
- Virtual Display Driver: https://github.com/VirtualDrivers/Virtual-Display-Driver
- RustDesk（仅作设计参考，本项目**没有**移植 RustDesk 的隐私模式）：https://github.com/rustdesk/rustdesk

## 版本及测试状态

v0.1：实现定时任务、登录校准、基础关屏、虚拟屏切换试验、120 秒回滚、状态查看、应急恢复和卸载。**已进行静态文件检查，未在 Windows 一体机、UU 远程和真实虚拟显示驱动上运行验证。请先用测试模式再激活。**
