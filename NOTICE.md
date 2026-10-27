# 第三方说明 / NOTICE

NightDisplay 以 MIT 许可发布。以下项目仅作为**设计参考**或**运行时可选下载**，本仓库未复制其源代码：

| 项目 | 许可 | 关系 |
|---|---|---|
| [RustDesk](https://github.com/rustdesk/rustdesk) 隐私模式 | AGPL-3.0 | 仅参考思路（虚拟屏为主屏、实体屏脱离桌面、防截屏黑幕的优先顺序）。NightDisplay 依据微软公开文档（QueryDisplayConfig / SetDisplayConfig / SetWindowDisplayAffinity）独立实现，**未复制任何代码**。 |
| [Virtual-Display-Driver (VDD)](https://github.com/VirtualDrivers/Virtual-Display-Driver) | MIT | 用户点击“安装虚拟显示驱动”时由 `tools/install_vdd.ps1` 从其官方 GitHub Release 下载并安装；驱动文件不随本仓库分发。 |
| [UUyc-companion](https://github.com/aoyamashou/UUyc-companion) (macOS 虚拟屏方案) | MIT | 仅参考思路；macOS 目前使用系统 pmset 关屏，并建议用 BetterDisplay 创建虚拟屏。 |

NightDisplay 与网易 UU 远程、ToDesk、RustDesk 均无隶属或授权关系；“UU 远程”“ToDesk”为其各自所有者的商标。本项目不注入、不修改任何远程控制客户端。
