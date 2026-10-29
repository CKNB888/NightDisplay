# 第三方说明

NightDisplay 采用 MIT 许可。项目独立实现 Windows 显示配置、黑幕、计划和恢复逻辑，不复制 RustDesk 的源代码。

## 随 Windows 安装包分发的驱动

Windows 产物包含 VirtualDrivers/Virtual-Display-Driver 的 25.7.23 x64 Driver Only 发布包。驱动采用 MIT 许可，许可证见 `resources/VDD-LICENSE.txt`（3.x 副本在 `legacy-python/nightdisplay/resources/`），打包时随程序保留。

上游：https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23

ZIP SHA-256：`e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a`。

源码仓库不提交驱动二进制；`tools/download_vdd.py` 从上游下载并校验。Windows 构建把校验后的文件放入应用内部供离线安装。默认 1080p/60 Hz 配置由 NightDisplay 单独提供，不修改上游的签名驱动文件。

## Python 运行依赖（仅 3.x）

打包产物还包含 Python、Tcl/Tk、Pillow、pystray 及其必要运行依赖。对应许可证由构建脚本从依赖包收集，构建清单应保留。Python 采用 PSF 许可，Tcl/Tk 采用其 BSD 风格许可，Pillow 采用 HPND 许可，pystray 采用 LGPL-3.0 许可。源码可用项目的 Python 包构建运行，替换依赖后可重新打包。

## 设计与商标

RustDesk 的虚拟屏和隐私模式只作为思路参考。Windows 接口依据微软公开文档实现。
4.0 起不再维护 macOS / Linux 路径，相关调研记录同样保留在 `legacy-python/`。

NightDisplay 与 UU 远程、ToDesk、RustDesk 没有隶属关系，不使用它们的标识或素材，也不注入或修改其客户端。

## Rust 依赖

4.x 只在运行时链接 Windows 系统库，不使用第三方 C/C++ 运行时之外的原生库。主要 crate：
`windows`（MIT/Apache-2.0）、`serde`/`serde_json`（MIT/Apache-2.0）、`chrono`（MIT/Apache-2.0）、
`tracing`/`tracing-subscriber`（MIT）、`anyhow`/`thiserror`（MIT/Apache-2.0）、
`fs4`（MIT/Apache-2.0）、`rand`（MIT/Apache-2.0）、`regex`（MIT/Apache-2.0）。
完整版本与许可证可以跑 `cargo tree` 复核。
