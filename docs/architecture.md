# NightDisplay 4 架构

本文写给要改这份代码的人。产品行为读 README；这里是**为什么这样分、边界在哪、
以后改的时候哪条线不能碰**。

- [1. 分层](#1-分层)
- [2. 运行时状态机](#2-运行时状态机)
- [3. 显示拓扑：一次原子切换](#3-显示拓扑一次原子切换)
- [4. 持久化：哪些文件、谁写、坏了怎么办](#4-持久化哪些文件谁写坏了怎么办)
- [5. IPC 协议](#5-ipc-协议)
- [6. 看门狗契约](#6-看门狗契约)
- [7. 界面（Tauri 2）](#7-界面tauri-2)
- [8. 日志与诊断包](#8-日志与诊断包)
- [9. Lint 策略](#9-lint-策略)
- [10. 测试策略](#10-测试策略)
- [11. 已知限制（诚实清单）](#11-已知限制诚实清单)

---

## 1. 分层

```text
crates/nd-core   纯逻辑：配置、时间计划、组合键、状态机、模式降级、会话记录、
                 IPC 报文格式、诊断包打包、ZIP。不链接任何 Windows API。
crates/nd-win    Windows 实现：显示配置、gamma、DDC/CI、黑幕窗口、键鼠钩子、
                 注册表自启、计划任务、命名管道、提升助手、看门狗、VDD。
crates/nd-gui    Tauri 2 界面：视图模型、命令层、托盘、窗口几何。
crates/nd-cli    唯一的可执行文件 NightDisplay.exe。参数解析 + 各动作 +
                 常驻进程（--action serve）；不带参数或 --action gui 时进界面。
```

为什么只有一个 exe：`nd-win::sysutil::exe_path()` 用 `current_exe()` 拼计划任务
和 Run 键的命令行。多一个可执行文件就要多处理一套"路径变了怎么办"；一个 exe
时，注册的动作永远是 `<这个 exe> --action xxx`。

`nd-core` 不依赖 `nd-win`，`nd-win` 不依赖 `nd-gui`，`nd-cli` 同时依赖三者。
依赖方向单向，`nd-core` 能在 ubuntu runner 上单独跑测试。

### 二进制里的薄壳

`main()` 只做三件事：非 Windows 直接报错退出、Windows 下调 `app::main_entry()`。
所有逻辑在库里，所以库能被测试、被界面复用。

---

## 2. 运行时状态机

配置里的持久状态只有一个枚举：

```rust
pub enum RunState { Day, Night }
```

外加两个"覆盖窗口"：

| 字段 | 含义 | 谁写 |
|---|---|---|
| `override_until` | 组合键解锁后，本段剩余时间保持白天 | `chord_unlock` |
| `hold_night_until` | 白天手动开夜间后，保持到下一次计划夜间结束 | `note_manual_night` |
| `recovery_pending` | 上次恢复没做完，下次 `reconcile` 必须先补 | `day()` 部分失败时 |

`night_mode: Option<Mode>` 记录**实际生效**的模式（`auto` 不会落盘）。
`state` 和 `night_mode` 必须一起看：`state == Night` 而 `night_mode == None`
说明记录损坏，`reconcile` 会当成白天处理并写日志。

### 正常路径

```text
                 ┌──────────── reconcile 每 2s/15s ─────────────┐
                 │                                              │
   Day ──night()──► Prewarm ──► SwitchRequested ──► Night ──day()──► Restore ──► Day
    ▲                 │              │                 │              │
    │                 │ 失败         │ 失败            │ 夜间修复     │ 部分失败
    └─────────────────┴──────────────┘                 │              ▼
                                        保持 Day，如实报错      recovery_pending = true
```

* **Prewarm**：`enter_virtual_screen(.., dry_run = true)` 只做
  `SDC_VALIDATE`，把虚拟屏以克隆模式排进路径集并校验。不应用任何东西。
* **SwitchRequested → Night**：一次 `SDC_APPLY`。这一步就是全部"切换"。
* **Night 期间的修复**：`ensure_night`（常驻进程每拍都调）只做幂等修补 ——
  黑幕还在就重新置顶、实体屏拿回桌面就再切一次虚拟屏、背光被拉回就再压一次、
  防睡眠续订。**绝不重新走一遍 night()**，因为那会重建窗口、产生第二次闪烁。

### 模式链

`mode::attempt_chain` 是**唯一**决定顺序的地方：

```rust
Auto          => [VirtualScreen, Overlay]
VirtualScreen => [VirtualScreen]
Overlay       => [Overlay]
ScreenOff     => [ScreenOff]
```

用户显式选过的模式不会被偷偷替换。`auto` 才允许降级，而且降级时
`NightReport.fallback_reason` 会带原因，界面和日志都如实显示。

### 进入夜间的顺序（`Engine::night`）

1. `validate_mode` 先看能力（没装 VDD 就选不了虚拟屏）；
2. 按链尝试 `enter_virtual_screen` / `enter_guard` / `screen_off`；
3. 成功那个把 `state = Night` 和 `night_mode` **一起**写进配置；
4. 附加层（保持唤醒、本地防触控、抑制锁屏）逐项执行，每项一条
   `StepRecord`；
5. 整个过程的步骤和耗时写进 `session-current.json`。

第 4 步失败不影响"已经是夜间"，但会记成 failed 步骤，
用户能在「运行日志 → 夜间会话」里看到是哪一步。

### 恢复（`Engine::day`）的顺序

刻意是**先恢复输入、再恢复显示**：

1. 丢掉键鼠钩子（不然用户点不动）；
2. 关掉锁屏抑制、取消防睡眠；
3. `exit_virtual_screen` / `exit_guard` / `screen_on`；
4. 从 `screen-state.json` 还原 gamma 和背光，切回原电源计划；
5. 清 `state`；任何一步失败 → `recovery_pending = true` 并返回错误。

顺序反了会出现"屏幕已经亮回来但鼠标还是死的"，那是最糟的中间态。

---

## 3. 显示拓扑：一次原子切换

这一节是整个项目最容易踩坑的地方，改之前请先读完。

### 为什么不用 `SDC_TOPOLOGY_*`

`SetDisplayConfig(.., SDC_TOPOLOGY_INTERNAL | SDC_APPLY)` 会让 Windows
自己挑一套拓扑。它可能顺带改分辨率、改主屏、改 DPI，而且**不同驱动行为不一致**。
我们要的是"只动一件事"。

所以：**永远自己给全量路径集**，用
`SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES`，并且：

* `SDC_VALIDATE` 先过一遍，不过就**什么都不做**；
* 通过后**只调一次** `SDC_APPLY`；
* 应用后重新 `QueryDisplayConfig` 确认活动 target 数 ≥ 1。

一次 `SDC_APPLY` 是"不闪"的物理前提：两次调用之间必然有一帧是中间态。

### 两条路径

```text
plan_arm(match_pat)     虚拟屏以 clone 模式排好、但不接管桌面
                        （Prewarm 用，dry_run 只校验）

plan_detach(match_pat)  虚拟屏接管桌面，实体屏保留 target 但清掉
                        PATH_ACTIVE —— 采集表面还在，桌面换源
```

`plan_arm` 和 `plan_detach` 产出的 `Vec<DISPLAYCONFIG_PATH_INFO>` 差**一个 flag**，
这也正是 Prewarm 有意义的原因：真正切换时驱动不用重新初始化模式。

`SwitchPlan` 里带的自检字段都是给测试和日志用的：

| 字段 | 用途 |
|---|---|
| `active_before` / `active_after` | 断言切换后不是零 target（零 target = 远程采集面消失 = 断流） |
| `mode_matched` | 虚拟屏分辨率和实体屏一致，合成器不用重建 |
| `shared_source` | 克隆模式下共用 source，切起来代价最小 |
| `vdd_identity` | 虚拟屏切换前后的 target 标识，用来确认采集表面没被动过 |

### 已知会闪的组合（诚实清单）

下面这些情况**没有**办法保证无闪，程序会照实报：

* 显卡驱动在 `SDC_APPLY` 内部重初始化输出（部分老 Intel 核显 + 外接 HDMI）；
* 实体屏和虚拟屏刷新率无法同时满足，`ALLOW_CHANGES` 逼驱动改模式；
* 多显卡（核显 + 独显各带屏）时跨适配器路径集被驱动拒绝；
* 系统里有第三方虚拟屏（UU/ToDesk 的虚拟显示器）且名字命中我们的匹配正则。

这些情况 `SDC_VALIDATE` 可能通过、`SDC_APPLY` 之后仍然有一帧黑 ——
唯一的保证是**不会**出现零 target。真出现无法避免的中断时，
`auto` 链会降级到 overlay 并在界面写明。

### 覆盖层的采集排除

overlay 模式的每一块屏是一个分层窗口，`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`
把它排除在采集之外，然后**回读**确认 `GetWindowDisplayAffinity` 真的返回了它。
回读不通过就拆掉窗口、报错，绝不谎报"已黑屏"。这条校验是 overlay 能不能算成功
的唯一判据。

---

## 4. 持久化：哪些文件、谁写、坏了怎么办

全部在 `%LOCALAPPDATA%\NightDisplay`（或 `NIGHTDISPLAY_HOME`）：

| 文件 | 写者 | 内容 | 坏了怎么办 |
|---|---|---|---|
| `config.json` | 所有进程（带文件锁） | 计划、模式、开关、运行状态 | 解析失败 → 用默认值 + 备份成 `config.json.bad` |
| `screen-state.json` | 夜间开始时**先**写 | gamma、背光、原电源计划、`topology_changed` | 看门狗读到就用；读不到只记日志 |
| `session-current.json` | 常驻进程 | 本次夜间每步耗时 | 忽略，下次覆盖 |
| `sessions.jsonl` | 常驻进程 | 最近 50 次会话 | 逐行解析，坏行跳过 |
| `onboarding.json` | 向导 | 每步结果、是否完成 | 坏了等于没做向导 |
| `ipc.json` | 常驻进程 | 管道名 + 32 字节令牌 + pid | 读不到就重新拉起常驻进程 |
| `helper-token.txt` | 提升助手 | 助手通道令牌 | 重新生成（助手会重启） |
| `daemon.lock` | 常驻进程 | 单实例锁（`LockFileEx`） | 进程退出即释放，不会变死锁 |
| `unlock.flag` | 组合键 | 本次解锁的标记 | 缺省视为未解锁 |
| `watchdog.json` | 常驻进程 | 正常退出标记 | 缺省视为"非正常退出"，看门狗按需要恢复 |
| `gui-window.json` | 界面 | 窗口位置/大小/最大化 | 不可见或小于 640×480 → 居中重开 |

### 写文件的顺序（关键）

`screens-state.json` 必须在**任何显示改动之前**落盘：

```text
1. 采集原始 gamma / 背光 / 电源计划
2. 写 screen-state.json（含 topology_changed = true 的预置）
3. SetDisplayConfig / SetDeviceGammaRamp / DDC 写
```

如果 2 之后、3 之前进程被杀，下次启动看到 `topology_changed = true`，
会走一遍还原 —— 白天还原是无操作，幂等。

所有配置写操作走 `Config::update(|c| ...)`，内部取
`state_lock::write_lock_timeout(..., 5s)`；拿不到锁返回中文错误，
不会静默丢写。

---

## 5. IPC 协议

### 客户端 ↔ 常驻进程

传输：Windows 命名管道，`PIPE_REJECT_REMOTE_CLIENTS`（远程机器连不上）。
报文：一行一条 JSON，`\n` 结束，**单条上限 8 KiB**。

请求：

```json
{"token":"<64 hex>","command":"status"}
{"token":"<64 hex>","command":{"action":"test","mode":"virtual_screen","seconds":15}}
```

回复：

```json
{"ok":true,"result":"白天 · 每日计划 20:00–10:30"}
```

* `token` 存在 `ipc.json`，只有本用户可读；比较用常量时间函数
  （`ipc::token_matches`），避免时序侧信道。
* 命令是**无版本的字符串白名单**，不是通用的远程调用。新增动作必须同时改
  常驻进程的 `handle`，GUI 侧只有 `commands::daemon_call` 一个入口。
* 超长报文直接断开连接；读取端先读满上限再判断，不会把内存吃满。

### 界面 ↔ 提升助手

助手是唯一以管理员身份跑的进程，常驻在后台（由向导一次 UAC 拉起）。
命令白名单：

```text
ping  install-vdd  uninstall-vdd  install-tasks  remove-tasks
install-watchdog  quit
```

`install-tasks` **只接受 `HH:MM` 时间点字符串**（`test_id` 字段传
`"20:00,10:30"`），不接受任何路径或命令行。这样即使令牌泄漏，
也没法借助手执行任意程序。

界面和 CLI 自己**永远不是管理员**；所有特权动作都转给助手。

---

## 6. 看门狗契约

看门狗是独立进程（`--action watchdog --parent <pid>`），不是线程。

为什么必须是进程：用户在任务管理器里结束主进程时，线程一起死，
gamma 会永久停在 0，屏幕看起来像显示器坏了。

### 主进程对看门狗的承诺

1. 启动时先 `watchdog::clear_clean_exit_marker()`，再拉起看门狗；
2. 正常退出（`--action day` / 托盘退出）前写 `watchdog.json` 标记；
3. 夜间状态先落盘 `screen-state.json`，之后才动显示。

### 看门狗对主进程的承诺

* 每 1 秒检查父进程 PID；启动后 3 秒内是宽限期，避免误判；
* 父进程消失 → 从 `screen-state.json` 还原 gamma、背光、电源计划、拓扑；
* 把配置写成白天；
* 如果需要，重启常驻进程；
* 看到正常退出标记就自己退出。

### 自启的三条路径

| 位置 | 触发 | 身份 | 为什么要有 |
|---|---|---|---|
| HKCU `Run` | 登录 | 当前用户（普通权限） | 最普通的一条，被安全软件拦的概率最低 |
| 计划任务 `NightDisplay_Logon` | 登录 | 当前用户 | `StartWhenAvailable`，能补跑错过的边界 |
| 计划任务 `NightDisplay_Boot` | 开机 | SYSTEM，最高权限，延迟 30s | 没人登录也能进入夜间（远程场景） |
| 计划任务 `NightDisplay_Watchdog` | 登录 | 当前用户 | 主进程被结束后自动恢复 |

单实例锁让三条路径同时触发也只剩一个进程，重复启动是安全的。
`autostart_actual()` 会**回读**三条路径的真实状态（含
`StartupApproved\Run` 的启用标记），界面显示"配置=开 实际=关"就说明被禁用了。

用户关掉自启后，程序**不会**再把它加回来：只有配置里的
`autostart_preference` 被用户显式打开才会写。

---

## 7. 界面（Tauri 2）

```text
crates/nd-gui
├── lib.rs          Builder、单实例插件、托盘、关窗进托盘、命令注册
├── commands.rs     19 个 #[tauri::command]，全部 async + spawn_blocking
├── state.rs        视图模型（纯数据，不碰平台，可在任意平台单测）
├── settings.rs     设置补丁的校验与落盘（先装计划任务，失败回滚）
├── tray.rs         托盘图标与菜单、显示主窗口、恢复几何
├── window_state.rs 窗口位置/大小持久化 + WindowLike 抽象
└── ui/             index.html / style.css / app.js（无打包器，withGlobalTauri）
```

### 为什么前端不用打包器

`frontendDist: "ui"` 直接指向静态目录。界面是六页固定布局，没有路由和组件复用
需求；一个 `app.js`（约 1000 行）配合 CSS 变量比引入 Vite + 框架更容易审计，
构建也少一个 Node 依赖链 —— CI 只需要 Rust 工具链。

### 界面不拥有引擎

所有黑屏动作都走 `commands::daemon_call` → 常驻进程。界面自己**不建窗口、
不装钩子、不改显示配置**。这带来三个直接结果：

* 关掉界面窗口（进托盘、甚至结束界面进程）不影响正在进行的夜间；
* 界面的内存占用只跟 WebView2 有关，跟引擎无关；
* 界面崩了不会把屏幕留在黑着的状态。

常驻进程不在时，`daemon_or_start` 先拉起 `--action serve` 再重试，
等待上限 8 秒。

### 视图模型与"每个数字都来自真实调用"

`state::build(cfg, caps, onboarding, now)` 是唯一把配置 + 能力 + 时间
揉成界面数据的地方。`Capabilities` 的采集走 `CAPS` 静态缓存，
TTL 30 秒 —— 背后是真 PowerShell 调用，界面上每秒刷一次不能每次都跑。

### 窗口几何

`window_state::Geometry` 存 `gui-window.json`。恢复时要求：

* 宽高 ≥ 640×480；
* 和任意一块显示器的可用区域相交（拔掉副屏后不会开到看不见的地方）。

不满足就居中。`WindowLike` trait 是为了让 `Window` 和 `WebviewWindow`
共用同一套代码 —— Tauri 2 的 `WebviewWindow` 没有公开的 `.window()`，
`get_window()` 又在 `unstable` 特性后面。

### 内存实测

**本仓库的 CI 只在 Linux 上跑 nd-core 测试，Windows runner 上没有可用的桌面会话，
界面无法真实启动，因此这里不给数字。** 需要一个真实 Windows 桌面上的测量：

```powershell
# 打开界面，等 10 秒稳定
$p = Start-Process .\NightDisplay.exe -PassThru
Start-Sleep 10
(Get-Process -Id $p.Id).WorkingSet64 / 1MB   # 界面 + WebView2 同进程
Get-Process msedgewebview2 | Measure-Object WorkingSet64 -Sum   # WebView2 子进程
```

预期构成：进程本身（Tauri + 系统 WebView2 加载器）几 MB 到十几 MB，
WebView2 渲染进程几十 MB。真实数字以实测为准，**不在这里编**。

---

## 8. 日志与诊断包

### 布局

```text
logs/daemon-2026-10-29.log      人读文本
logs/daemon-2026-10-29.jsonl    机器读（level/target/fields/trace_id）
logs/gui-…  logs/helper-…  logs/watchdog-…  logs/installer-…
logs/traceback.log              只有出错时追加完整错误链
```

保留 14 天；总量超 64 MiB 从最旧的删。写入失败**绝不影响**显示动作。

### 会话记录

`session-current.json` / `sessions.jsonl` 记录一次夜间的每个步骤：

`prewarm`、`topology`、`mode_match`、`settle`、`active_paths`、`overlay`、
`gamma`、`brightness`、`keep_awake`、`input_hook`、`lock_suppression`、
`restore`。

每步有 `status`（ok/failed/skipped）、`ms`、`detail`。
界面「运行日志 → 夜间会话」按这套 ID 渲染中文标签，跳过和失败分开显示。
trace id 同时出现在对应日志行里，能一键对上。

### 诊断包

`--action export-diag` / 界面「导出诊断包」生成 ZIP：

* `manifest.txt`、`selfcheck.json`、`config.json`（已脱敏）、
  `sessions.jsonl`（最近 5 次）、`logs/<组件>-<日期>.*`；
* 脱敏规则：本机 home 绝对路径 → `<home>`，64 位十六进制 → `<token>`，
  用户名 → `<user>`；
* 不含远程软件账号、不含桌面内容、不含任何令牌原文。

ZIP 由 `nd-core::zip` 自己实现（store 模式），不引第三方压缩库 —— 包里有几千行
日志，store 足够，也少一个 C 依赖。

---

## 9. Lint 策略

CI 跑：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
```

工作区在 `Cargo.toml` 里设 `[workspace.lints.rust] unsafe_op_in_unsafe_fn = "deny"`。
`missing_docs` **故意不开**：本项目所有文档是中文，逐条 `///` 会写成一堆
"返回 xxx"，对读代码没有帮助；公开接口在 `docs/architecture.md` 和
模块级 `//!` 里讲。

下面是全部 allow 及其理由。每条都写在对应 crate 根部的 `#![allow(...)]` 里。

| Lint | 允许的 crate | 理由 |
|---|---|---|
| `clippy::missing_errors_doc` | core, win, gui, cli | 所有失败函数只被本仓库自己的二进制调用，逐函数写 `# Errors` 只是把类型再抄一遍 |
| `clippy::missing_panics_doc` | core, win, gui, cli | 运行时路径上不 `unwrap`/`expect`（见下），剩下的 panic 只在测试和 `Default` 里的常量解析 |
| `clippy::must_use_candidate` | core, win, gui, cli | 小 `Copy` 枚举上的谓词有六十多个，逐个加 `#[must_use]` 是噪声 |
| `clippy::module_name_repetitions` | core, win, gui, cli | 模块名刻意重复领域词（`mode::Mode`）；改成 `ModeKind` 在每个调用点都更难读 |
| `clippy::struct_excessive_bools` | core, win, gui, cli | `Capabilities`/`SelfCheck`/`Registration` 本来就是一组独立的 Win32 事实；命令行 `Args` 天然是一堆开关 |
| `clippy::too_many_lines` | core, win, gui, cli | 夜间的状态机是线性的、逐段有注释；拆成六个带六个参数的辅助函数更难审计 |
| `clippy::doc_markdown` | core, win, gui, cli | 文档是中文散文，给每个英文词加反引号只会更难读 |
| `clippy::cast_possible_truncation` | core, win, gui | 时间换算是 `u32`/`i64` 分钟秒，取值范围由 `MinuteOfDay` 校验；`DISPLAYCONFIG_*` 字段本身就是 `u32`/`i32` |
| `clippy::cast_sign_loss` | core, win, gui | 同上，且都在转换前做过范围检查 |
| `clippy::cast_possible_wrap` | core, win | LUID 的高低位、`SetWindowLongPtr` 的 `isize`，都是 ABI 定义的位宽 |
| `clippy::cast_lossless` | core, win, gui | 显式 `as` 在这里是刻意的位宽标注（例如 `u32 as u64` 表示"这是同一条时间轴"） |
| `clippy::cast_precision_loss` | gui | 倒计时环计算 `f64` 弧度，数值范围 < 1e6，精度足够 |
| `clippy::field_reassign_with_default`（仅 `cfg(test)`） | core, win, gui | 测试里逐字段赋值更贴近"用户只改了一项"的场景 |
| `clippy::wildcard_imports`（仅 `engine::testing` 模块） | core | `FakePlatform` 的辅助函数和主模块共用一个 `use`，测试里通配更好读 |

`unused` 在 `nd-win` 的非 Windows 构建里允许（整包 `#![cfg_attr(not(windows), allow(unused))]`），
这样 `cargo check` 在 Linux 上也能当语法检查用。

### 运行时路径上的其他硬约束

* 库用 `thiserror`（`nd_core::NdError`），二进制用 `anyhow` + `context`；
* 每个 `unsafe` 块前面必须有 `// SAFETY:` 说明为什么这套参数安全；
* Win32 调用一律包成小的类型安全包装，句柄用 `Drop` 释放
  （`OwnedHandle`、`RegKey`、`DcHandle`）；
* Windows 消息循环里不 `panic!`：钩子回调和窗口过程用
  `if let`/`match` 兜底，错误写日志。

---

## 10. 测试策略

| 层 | 在哪跑 | 内容 |
|---|---|---|
| `nd-core` 单元测试 | ubuntu + windows | 时间计划边界、组合键解析、配置迁移、模式降级、会话记录、诊断脱敏、IPC 编解码 |
| `nd-win` 单元测试 | windows only | 路径集构造（`plan_arm`/`plan_detach` 的不变量）、注册表读写、任务计划命名、DDC 解析 |
| `nd-gui` 单元测试 | windows only | 视图模型、窗口几何、设置补丁回滚 |
| 冒烟 | windows CI | `--version`/`--help`/`status`/`env`/`selfcheck`/`setup`/`onboarding`，用独立 `NIGHTDISPLAY_HOME` |

### 纯逻辑测试为什么能在 Linux 上跑

`Engine` 的平台动作全部通过 `Platform` trait 注入，测试用 `FakePlatform`
记录调用序列。它能模拟：能力缺失、黑幕失败、虚拟屏切换失败、恢复失败、
能力指纹变化。所以"模式降级链"、"夜间失败不改状态"、"恢复失败置
`recovery_pending`"这些行为都在纯逻辑层被钉住，不依赖真机。

### 显示路径的不变量测试

`plan_arm`/`plan_detach` 的单元测试直接对 `SwitchPlan` 断言：

1. `active_after >= 1` —— 永远不产生零 target 的路径集；
2. 实体屏路径在 `plan_detach` 里必须清掉 `PATH_ACTIVE`，虚拟屏必须置上；
3. 虚拟屏模式和实体屏一致时 `mode_matched == true`；
4. 路径集里出现的每个 target 都在 `QueryDisplayConfig` 的原始集合里
   （不凭空造 target）。

这些测试在 windows runner 上跑，因为 `DISPLAYCONFIG_*` 结构来自 `windows` crate。
它们不调用 `SDC_APPLY`，所以不需要真实显示器。

---

## 11. 已知限制（诚实清单）

* **`SDC_APPLY` 不保证零中断**：上面第 3 节列的组合驱动可能仍有一帧黑。
  程序能保证的是"提交一次、不产生零 target、失败不动任何东西"。
* **DDC/CI 背光**：一体机、部分笔记本内屏不支持；这类屏夜间是纯黑但背光仍亮。
  自检逐块点名。
* **overlay 的采集排除**：`WDA_EXCLUDEFROMCAPTURE` 需要 Win10 2004+。
  系统接受了请求不代表每种采集方式都兼容；UU/ToDesk/RDP 请各自实测一次。
* **断电/强制关机**：看门狗也活不了。下次启动时 `recover_after_crash`
  会把屏幕救回来。
* **CI 没在真实一体机上验证过**，也没法在 CI 里验证远程会话。
* **界面内存**：见第 7 节，需要真实 Windows 桌面实测，这里不给数字。
