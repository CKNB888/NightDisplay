# 3.0.2 本地审计与验证

本轮基于 main 的 `4d8b88c`，没有套用上一轮的字符串替换修复脚本。

## 修改范围

- Windows 显示 API 参数、临时拓扑、目标校验、多级恢复、虚拟屏选择顺序。
- 独立恢复进程的测试会话 ID、失败状态、程序退出后的打包资源隔离。
- 跨进程配置锁、完整文件替换、损坏配置和显示备份的处理。
- 按数据目录隔离的 IPC，超时不重复进入夜间，界面线程统一分发。
- 临时 Windows 电源计划、合盖与自动睡眠设置、原计划恢复、唤醒进程心跳。
- 固定校验值的 VDD 驱动安装、真实安装返回码、默认一块 1080p/60 Hz 虚拟屏。
- 磨砂卡片主界面、多时段、组合键录制、自启、更新检查、作者与赞助信息。
- wheel 打包、Pillow 动态 Tk 模块、Windows 安装器、macOS DMG、产物 QA 与发布门槛。

## 已执行检查

| 检查 | 结果 |
|---|---|
| Python 回归测试 | 72 passed，4 项真实 Windows API 测试因 Linux 平台跳过 |
| Python 未定义名称与语法检查 | 通过 |
| git diff --check | 通过 |
| wheel 构建 | 通过，包含驱动安装脚本、默认配置和许可证 |
| Linux Xvfb 源码 GUI | 通过，检查中文字体与设置窗口 |
| Linux Xvfb 显示备份和恢复 | 通过，恢复级别 1 |
| Linux onefile CLI 与 GUI | 已实际构建与启动；打包 GUI 中遗漏的 Pillow 动态模块已修复 |
| workflow YAML 与发布依赖 | 通过静态检查，release 依赖打包产物的 Windows QA |
| VDD 下载 ZIP | 下载并校验 SHA-256，与固定上游发布文件一致 |

Linux 容器没有可用的用户 systemd/logind 总线，Xvfb 没有实体屏 DPMS。这些检查明确记为 SKIP，没有记为通过。

## Windows 与 macOS 的边界

已下载微软官方 Windows 11 Enterprise LTSC 评估镜像，并在本地安装 QEMU。宿主环境没有 `/dev/kvm`，只能使用软件模拟。尝试两组 CPU/时钟配置后，Windows 11 安装环境仍停留在启动阶段，尚未进入可以运行 NightDisplay 的桌面。这个状态不能算 Windows 测试通过。

Windows 10 官方评估镜像的下载路径已核对；本次打包时尚未完成安装，也未运行 Windows 程序测试。

macOS 没有本地运行环境。菜单栏、自启、输入监控权限和基础关屏仍需 macOS runner 与真机验证。macOS 没有内置虚拟屏驱动，基础关屏会被键鼠唤醒。

UU/ToDesk 的实际远程画面、一体机背光、驱动更新或拔插显示器后的行为，没有真实会话与设备，无法在本地容器确认。

## 推送与发布状态

当前 GitHub 连接对 `CKNB888/NightDisplay` 返回 `push: false`。命令行 Git 推送预检也因没有凭据失败。因此没有推送远端分支，没有创建 PR，没有合并，没有触发本轮 Actions，也没有创建正式 Release。

本轮代码是待平台验证的修复候选。发布前应完成 Windows 打包产物 QA 和 macOS 检查，不能因为本地单元测试通过就改称稳定商业成品。
