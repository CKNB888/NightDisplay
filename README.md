# NightDisplay 夜间黑屏 · 远程照常可用

> 每天到点让电脑**本地屏幕变黑**，电脑保持开机，**UU 远程 / ToDesk / RustDesk 等远程软件照常连接操作**；到点自动恢复。
> 适合一体机、办公室电脑、家里常开的主机：晚上不亮屏、不被旁人看到，远程随时能用。

**开发与赞助：传康kk，联系方式 1837620622**

> 当前分支 `dev` 为 2.0 开发版（未发布 Release）。稳定版 1.0 见 `main` 分支与 Releases。

---

## 功能一览

| 功能 | Windows | macOS | Linux |
|---|:-:|:-:|:-:|
| 每日自定义时间段（默认 20:00 → 10:30，可跨午夜） | ✅ 计划任务 | ✅ launchd | ✅ systemd 用户定时器 |
| 只保留虚拟屏（最接近 UU“关闭显示屏”） | ✅ 需虚拟显示驱动 | ⚠️ 需 BetterDisplay 等 | ✅ X11 + 虚拟输出 |
| 防截屏黑幕（本地黑、远程画面不黑） | ✅ Win10 2004+ | — | — |
| 基础关屏 | ✅ | ✅ pmset | ✅ xset dpms（X11） |
| 夜间阻止系统睡眠 | ✅ SetThreadExecutionState | ✅ caffeinate | ✅ systemd-inhibit |
| 切换前完整备份显示配置，失败分级回滚 | ✅ | — | ✅ |
| 应急恢复（托盘 / 快捷键 Ctrl+Alt+Shift+R / 命令行） | ✅ | ✅ | ✅ |
| 图形界面（侧边导航、状态卡、浅/深色） | ✅ | ✅ | ✅ |

## 显示模式说明

| 模式 | 效果 | 远程看到的画面 | 本地动鼠标 |
|---|---|---|---|
| **自动（默认）** | 有虚拟屏→只保留虚拟屏；否则 Win10 2004+ 用防窥黑幕；否则基础关屏 | 正常 | 视实际模式 |
| 只保留虚拟屏 | 实体屏从桌面移除（不输出信号，一体机通常背光也会关） | 正常（远程看到的是虚拟屏） | 不会亮 |
| 防窥黑幕 | 全屏黑色窗口，已设置“不被截屏/录屏”，键鼠穿透 | 正常 | 不会亮 |
| 基础关屏 | 系统关显示器 | 正常 | **会亮** |

“只保留虚拟屏”需要一块虚拟显示器：
- **Windows**：在“显示模式”页点“安装虚拟显示驱动”（会弹出管理员确认），安装开源 [Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)（MIT）。
- **macOS**：可用 BetterDisplay 创建虚拟屏，再配合系统设置；当前版本默认基础关屏。
- **Linux**：X11 下需要 `VIRTUAL*` / `DUMMY*` / evdi 虚拟输出；Wayland 不支持改输出，请切换到 X11 会话。

> 与 UU 远程的关系：UU 的“超级屏”会关闭实体屏且与 UU 虚拟屏互斥；UU 虚拟屏断开连接会自动删除。所以 NightDisplay 推荐使用**自己安装的常驻虚拟屏**，而不是依赖 UU 临时虚拟屏。UU 自带的“防窥模式 → 关闭显示屏”（Ctrl+Alt+P 解除）也可与本工具互补使用。

## 快速开始

1. 下载对应系统的压缩包（Actions 构建产物 / Release），解压。
   - Windows：双击 `NightDisplay.exe`
   - macOS：打开 `NightDisplay.app`（首次需右键→打开）
   - Linux：`./NightDisplay`（需要 X11）
2. **定时计划**页设置“进入夜间 / 恢复显示”时间，保存。
3. **显示模式**页选择模式（推荐“自动”），点“测试（120 秒自动恢复）”。**请先确认远程软件已能连上，并有人在电脑旁。**
4. 测试成功后回到首页，打开“每日自动计划”开关。（24 小时内没有成功测试过当前模式，不允许开启，防止远程把自己锁在外面。）

## 安全设计

- 不拦截键盘鼠标，不需要关闭杀毒，不注入任何远程软件，不修改 UU / ToDesk 客户端。
- 每次进入夜间前把当前显示配置**完整备份**（保留最近 10 份 + 最近一次正常配置）。
- 恢复按顺序尝试：完整备份 → 仅拓扑 → 仅内屏 → 扩展 → 复制，任何一级成功即停止。
- 切换后立即校验，失败**自动回滚**。
- 应急恢复：托盘菜单 / `Ctrl+Alt+Shift+R` / 命令行 `NightDisplay --action emergency`（恢复显示并暂停计划）。
- 一键卸载：设置页“卸载计划与自启”或 `--action uninstall`。

## 命令行

```
NightDisplay --action status          查看状态
NightDisplay --action night [--mode M] 立即进入夜间（M: auto/virtual_only/overlay/screen_off）
NightDisplay --action day             立即恢复
NightDisplay --action test --mode M --seconds 120   测试并自动恢复
NightDisplay --action activate --start 20:00 --end 10:30   开启每日计划
NightDisplay --action pause | emergency | uninstall
NightDisplay --action displays        列出显示器（Windows/Linux）
NightDisplay --action plan-virtual    预演“只保留虚拟屏”（只校验，不应用）
NightDisplay --action install-vdd     安装虚拟显示驱动（Windows，管理员）
```

源码运行：`python -m nightdisplay`（Python 3.9+，需要 tkinter；可选 `pip install pystray pillow` 启用托盘）。

## 数据位置

| 系统 | 目录 |
|---|---|
| Windows | `%LOCALAPPDATA%\NightDisplay` |
| macOS | `~/Library/Application Support/NightDisplay` |
| Linux | `~/.local/state/nightdisplay` |

包含 `config.json`、`backups/`（显示配置备份）、`logs/`（保留 30 天）。

## 测试

```
python -m pytest tests            # 时间逻辑、配置、xrandr 解析、systemd/launchd 生成、Windows 结构体与备份还原
python tools/ci_integration.py    # 真机集成：计划任务注册/删除、防睡眠、显示备份还原、界面自检（Windows 加 --vdd）
```

GitHub Actions 在 Windows（latest/2022）、macOS（arm64/Intel）、Ubuntu 上自动跑全部测试并打包。

**仍需真机验证**：UU 远程连接后的实际画面、一体机背光是否真正熄灭、与 UU“超级屏/防窥模式”同时使用时的表现。CI 云主机没有真实显示器和 UU 客户端，无法覆盖这些。

## 常见问题

- **远程连上是黑的？** 远程端切换到虚拟屏画面；或按 Ctrl+Alt+Shift+R / 托盘“应急恢复”。
- **本地动鼠标屏幕亮了？** 你在用“基础关屏”模式，换成“只保留虚拟屏”或“防窥黑幕”。
- **电脑夜里睡眠了？** 确认设置里“夜间阻止系统睡眠”开启；笔记本合盖仍可能睡眠。

## 许可与致谢

MIT，见 [LICENSE](LICENSE)。第三方参考与商标说明见 [NOTICE.md](NOTICE.md)。本项目为独立工具，与网易 UU 远程、ToDesk 无隶属关系。

开发与赞助：传康kk，联系方式 1837620622
