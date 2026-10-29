# NightDisplay 4

按计划把这台电脑的屏幕变黑 —— **本机看是纯黑，远程软件照常能看到桌面、照常能操作，画面不冻也不闪。**

> **平台**：只支持 **Windows 10 2004（build 19041）及以上，x64**。
> 4.0 是 Rust 重写，macOS 和 Linux 支持取消了 —— 旧版在这两个平台上从来没真正做到
> "本地黑屏、远程正常"，与其留半成品，不如把 Windows 这一条路做扎实。
> 3.x 的代码保留在 [`legacy-python/`](legacy-python/)。

作者：传康Kk。赞助及联系：1837620622。
[下载](https://github.com/CKNB888/NightDisplay/releases/latest) · [反馈](https://github.com/CKNB888/NightDisplay/issues)

---

## 一、它是怎么黑的

4.0 的主路径是**虚拟屏**，不是"盖一层窗口"。

### 默认模式：虚拟屏接管（`virtual_screen`）

程序随包带一个虚拟显示驱动（VirtualDrivers/Virtual-Display-Driver，MIT）。
安装向导会把它静默装好，虚拟屏从此常驻。

白天：虚拟屏以**复制模式**和实体屏共用同一个桌面 —— 桌面上什么都看不到，也不多一块屏。

晚上：**只发一次 `SetDisplayConfig`**，提交一份完整的路径集：

* 虚拟屏那条路径**原样保留**（source id、target id、模式索引全部不动）；
* 实体屏的 target 仍然留在配置里（设备还在、还枚举得到），只是**不再接着桌面**；
* 提交前先 `SDC_VALIDATE` 校验，不通过就什么都不改；
* 提交后活动 target 数**永远 ≥ 1**。

结果：显示器真的没有画面（不是"看起来黑"，是"没有输出"），
而远程软件抓着的那块采集表面一直活着，所以**不冻、不闪**。

这就是旧版两个老毛病（"点黑屏远程就卡住"、"远程进来先亮一秒"）的根因修法：
旧版把实体屏的 target 整个删掉、并且分两步调用。

### 自动降级：黑幕（`overlay`）

某些显卡/驱动组合确实做不到无中断切换（见文末"已知限制"）。
这时默认模式 `auto` 会自动降级到黑幕：

* 每块屏盖一个纯黑窗口，用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`
  把它排除在采集之外，然后**回读校验**系统真的接受了 —— 校验不过就拆掉、报错，
  绝不谎报"已黑屏"；
* 再加 gamma 归零和 DDC/CI 背光归零（这两层尽力而为，逐块报告）。

降级会写进日志和界面，不会偷偷换掉机制。

### 手动选项：关屏（`screen_off`）

直接让显示器休眠。**动鼠标就亮**，只能自己选。

### 模式顺序是确定的

```text
auto          → 先 virtual_screen，不行才 overlay
virtual_screen→ 只用虚拟屏（失败就失败，不降级）
overlay       → 只用黑幕
screen_off    → 只用关屏
```

用户显式选过的模式不会被偷偷替换。

### 附加层（都不影响夜间算不算成功，逐项报告）

| 层 | 做什么 | 失败时 |
|---|---|---|
| gamma | 每个输出 gamma ramp 归零，屏幕更黑 | 记日志，跳过 |
| 背光 | DDC/CI VCP `0x10` 设为 0，每 5 秒重压一次 | 一体机/部分笔记本内屏不支持，属于正常限制 |
| 保持唤醒 | 夜间不让系统睡眠 | 记日志 |
| 本地防触控 | 吞掉**真实硬件**键鼠事件，放行注入事件 | 记日志 |
| 抑制锁屏 | 每 60 秒重置空闲计时，避免登录界面盖住屏幕 | 记日志 |

> **安全权衡**：抑制锁屏意味着夜间这台电脑**不会因为闲置自动锁屏**。
> 如果你所在的场景对物理安全敏感，请在设置里关掉「夜间抑制锁屏」。
> 程序不绕过任何组策略强制锁屏；组策略强制时锁屏界面会盖住屏幕，属预期行为。

---

## 二、第一次运行：一个向导，一次授权

首次打开会自动弹出向导。**全程只弹一次管理员确认（UAC）**

| 步骤 | 做什么 | 怎么报告 |
|---|---|---|
| 1 授权并准备后台服务 | 拉起一个常驻的**提升助手**，之后所有需要管理员的事都交给它 | 成功 / 你点了"否"（附日志链接） |
| 2 虚拟显示驱动 | `pnputil` 静默安装包内**自带**的驱动，然后**验证虚拟屏真的出现在显示配置里** | 这一步不过，向导**不允许完成** |
| 3 开机自启与看门狗 | HKCU Run + 登录计划任务 + 开机计划任务 + 看门狗任务，逐项回读 | 被安全软件拦下时给出确切手动命令 |
| 4 本地防触控与锁屏 | 写配置并立刻验证这条链路能用 | 失败时说明哪一项、去哪里手动开 |
| 5 15 秒自检 | **真的黑屏 15 秒**再自动恢复，附远程连接检查提示 | 每块屏、首帧延迟、gamma/背光的真实结果 |
| 6 确认远程能连 | 勾选「已确认远程能连」，向导结束 | 勾了才算完成 |

规则：

* 每步都报**真实结果**：`成功` / `已跳过` / `需要手动处理（附确切命令或链接）`，
  不会只给一句"失败了"。
* 完成状态写在 `onboarding.json`，之后**永不再自动弹出**。
* 任何一步失败，在设置里点 **「重新授权 / 修复」**（`NightDisplay.exe --action repair`），
  它**只重跑失败的那几步**。
* 驱动这一步失败时，向导给出可照抄的命令：

  ```text
  pnputil /add-driver "<安装目录>\resources\vdd\MttVDD.inf" /install
  设备管理器 → 操作 → 添加过时硬件 → 显示适配器 → 从磁盘安装 → 选同一个 .inf
  ```

### 为什么是"一个提升助手"

* 界面和命令行**自己永远不以管理员身份运行**；
* 需要管理员的事（装驱动、登记计划任务、装看门狗）全部转给一个提升助手进程；
* 助手的 IPC 是**白名单**：`ping`、`install-vdd`、`uninstall-vdd`、`install-tasks`、
  `remove-tasks`、`install-watchdog`、`quit`，别的什么都不干；
* 命名管道 + 32 字节随机令牌（只有本用户可读），`PIPE_REJECT_REMOTE_CLIENTS`，
  远程机器连不上；
* `install-tasks` 只接受 `HH:MM` 时间点字符串，**不接受任何路径或命令行** ——
  即使令牌泄漏也没法借助手执行任意程序。

---

## 三、装好之后

```text
NightDisplay.exe --action test --seconds 15               黑 15 秒再自己亮回来
NightDisplay.exe --action activate --start 20:00 --end 10:30   启用每日计划
NightDisplay.exe --action status                          看当前状态
NightDisplay.exe --action pause                           暂停每日计划（不改屏幕）
NightDisplay.exe --action emergency                       立刻恢复并暂停（可远程执行）
NightDisplay.exe --action gui                             打开界面（不带参数也一样）
NightDisplay.exe --action serve                           常驻进程（自启都调它）
```

启用计划必须先有一次**成功的测试**：测试结束、屏幕自己亮回来才算数；
超过一天、或者换了显示器/显卡驱动（能力指纹变了），都要重测。

### 怎么亮回来（四条路，都不需要"远程能点按钮"）

1. **本机按住 C + K 约 0.3 秒**（可改键、可改时长）。解锁后本段剩余时间不再黑屏。
2. **远程执行** `NightDisplay.exe --action emergency`，随时有效。
3. **计划到点自动恢复**。
4. **看门狗**：主进程被结束后几秒内把 gamma、背光、拓扑、电源计划全部还原。

### 三条自启路径

| 位置 | 触发 | 身份 |
|---|---|---|
| HKCU `Run` | 登录 | 当前用户 |
| 计划任务 `NightDisplay_Logon` | 登录 | 当前用户（`StartWhenAvailable`，能补跑错过的边界） |
| 计划任务 `NightDisplay_Boot` | 开机 | SYSTEM，最高权限，延迟 30 秒 |
| 计划任务 `NightDisplay_Watchdog` | 登录 | 当前用户 |

四条同时登记，单实例锁让它们互相幂等。**你关掉自启之后，程序不会再把它加回来。**

`--action status` 会显示"配置=开 实际=?"：实际=关 说明被「启动」项或安全软件禁用了。

### 看门狗与崩溃恢复

* 主进程被任务管理器结束 → 看门狗几秒内从 `screen-state.json` 还原
  gamma、背光、电源计划、显示拓扑；
* 关机/注销 → 程序先还原屏幕，下次开机是正常桌面；
* 断电/强制关机 → 下次启动 `recover_after_crash` 把屏幕救回来。

`screen-state.json` 在**任何显示改动之前**就落盘，所以"改到一半进程没了"也能还原。

---

## 四、界面

Tauri 2 + 系统自带 WebView2，深色，左侧边栏：

| 页面 | 内容 |
|---|---|
| 首页 | 倒计时环（今晚还有多久进入夜间）、当前状态、这台机器能做什么、需要处理的警告 |
| 定时计划 | 开始/结束时间、额外时间段、启用开关 |
| 显示模式 | 四种模式卡片 + 这台机器的能力（虚拟屏是否就绪、采集排除是否可用、DDC 支持） |
| 运行日志 | **夜间会话**（最近一次每步的耗时和结果）/ **日志文件**（按组件浏览）；导出诊断包 |
| 设置 | 自启、本地防触控、抑制锁屏、解锁组合键与时长、保持唤醒、夜间高性能、重新授权/修复、自检、路径 |
| 关于 | 版本、许可、仓库 |

* 自定义开关、60fps 过渡、toast 提示；100%–200% 缩放都清晰；
* 托盘菜单：立即黑屏 / 恢复显示 / 暂停今天的计划 / 打开设置 / 退出；
* 关窗 = 收进托盘，**引擎在常驻进程里，界面退出不影响夜间**；
* 记住窗口大小位置；拔掉副屏后不会开到看不见的地方；
* 单实例：第二次启动只是把已有窗口叫到前台。

### 诊断

**「运行日志 → 夜间会话」**按步骤显示上一次夜间：预热、拓扑、模式匹配、落定、
活动路径、黑幕、gamma、背光、保持唤醒、防触控、抑制锁屏、恢复 ——
每一步的真实结果和毫秒数，失败与跳过分开显示。

**「导出诊断包」**生成一个 zip：

```text
manifest.txt          包里有什么、什么时候导出的
selfcheck.json        显卡、驱动、每块屏的真实模式、DDC 支持
config.json           配置（路径和令牌已脱敏）
sessions.jsonl        最近 5 次夜间会话，每步耗时
logs/<组件>-<日期>.*  按组件分开的日志
```

不含远程软件账号、不含桌面内容。
`NightDisplay.exe --action export-diag [--out <目录>]` 命令行也能导。
`--action selfcheck` 打印 JSON + 中文摘要；`--debug` 提高日志级别。

---

## 五、命令行

| 动作 | 说明 |
|---|---|
| `gui`（默认） | 打开界面 |
| `setup` / `repair` / `onboarding` | 向导 / 只重跑失败步骤 / 打印每步真实结果 |
| `serve` | 常驻进程：定时对齐 + 本地 IPC + 组合键 |
| `status` / `selfcheck` / `env` / `displays` | 诊断 |
| `night` / `day` / `reconcile` | 手动进夜间 / 恢复白天 / 按计划对齐一次 |
| `test` / `activate` / `pause` | 测试 / 启用 / 暂停 |
| `emergency` / `uninstall` | 应急恢复 / 卸载（恢复 + 删任务 + 关自启） |
| `plan-virtual` | **只演练**虚拟屏切换，不改任何显示设置 |
| `backup` / `restore` | 备份 / 还原显示配置 |
| `install-vdd` / `uninstall-vdd` | 安装 / 卸载内置虚拟显示驱动 |
| `export-diag` | 导出诊断包 |
| `awake` / `update` | 保持唤醒 N 秒 / 检查新版本 |
| `watchdog` / `helper` / `quit` | 内部使用 |

选项：`--mode`、`--start`、`--end`、`--delay`、`--seconds`、`--out`、`--only`、
`--debug`、`--tray`、`--home` 等。
退出码：`0` 成功 / `1` 出错（屏幕会尽量保持安全）/ `2` 黑屏锁定中拒绝本地恢复。

---

## 六、数据目录

默认 `%LOCALAPPDATA%\NightDisplay`（`--home` 或 `NIGHTDISPLAY_HOME` 可覆盖）：

| 文件 | 内容 |
|---|---|
| `config.json` | 计划、模式、开关、运行状态 |
| `onboarding.json` | 向导是否完成、每步真实结果 |
| `screen-state.json` | 夜间开始时的 gamma、背光、电源计划、拓扑快照（看门狗靠它恢复） |
| `session-current.json` / `sessions.jsonl` | 本次 / 最近 50 次夜间会话，每步耗时 |
| `selfcheck.json` | 最近一次自检 |
| `ipc.json` / `helper-token.txt` | 常驻进程 / 提升助手的令牌与管道名 |
| `daemon.lock` | 单实例锁 |
| `gui-window.json` | 界面窗口位置和大小 |
| `backups/` | 显示配置备份（最近十份轮转） |
| `logs/<组件>-YYYY-MM-DD.log` / `.jsonl` | 按组件 + 按天，保留 14 天，总量上限 64 MiB |
| `logs/traceback.log` | 完整错误链 |

---

## 七、出问题时

| 现象 | 先做什么 |
|---|---|
| 远程在黑屏期间看不到画面 | 跑 `--action selfcheck`。虚拟屏没装好、或驱动拒绝切换，自检会点名 |
| 屏幕黑了但背光还亮着 | 正常限制：这块面板不支持 DDC/CI，自检里会点名是哪块 |
| 黑屏时远程顿了一下 | 这台机器的驱动做不到无闪切换（见"已知限制"）。日志里 `topology` 那一步会有 `apply` 耗时和降级原因 |
| 提示"提权助手没启动" | UAC 里点了"否"或被杀软拦了。点「重新授权 / 修复」 |
| 计划任务登记失败 | 向导会给确切手动命令；常见原因是组策略或安全软件 |
| 屏幕一直黑着，什么都做不了 | 本机按住 C + K；或远程执行 `--action emergency`；或等看门狗几秒内恢复 |
| 开机没进夜间 | `--action status` 看"开机自启：配置=开 实际=?" |
| 界面打不开 | Win10 上可能缺 WebView2。装一次微软 Evergreen Runtime |

报问题时给「导出诊断包」的 zip（已脱敏）+ 远程软件名称 + 显卡型号就够了，
**不需要提供远程账号或密码**。

---

## 八、能保证什么，不能保证什么

**能保证**：

* 显示配置**一次提交**；`SDC_VALIDATE` 不过就什么都不改；
* 提交后活动 target 数**永远 ≥ 1**，显示数量不变；
* 虚拟屏的 target 身份不变（采集表面不会被重建）；
* 黑幕如果没能确认"已排除在采集之外"会立刻拆掉并报错，绝不假装成功；
* gamma、背光、电源计划、拓扑的原始值都先存盘，主进程被杀有看门狗兜底。

**不能保证**：

* **不是所有显卡组合都能零闪切换**。下面这些仍可能有一帧黑：
  驱动在 `SDC_APPLY` 内部重初始化输出（部分老 Intel 核显 + HDMI 外接）；
  实体屏和虚拟屏刷新率无法同时满足；多显卡跨适配器路径集被驱动拒绝；
  系统里已有第三方虚拟屏（UU/ToDesk）且命中匹配规则。
* **DDC/CI 背光**在一体机和部分笔记本内屏不可用，这类机器夜间纯黑但背光仍亮。
* `WDA_EXCLUDEFROMCAPTURE` 只是 Windows 接受了请求，**不等于每种采集方式都兼容**。
  UU、ToDesk、RDP 请各试一次（向导第 5 步就是为这个准备的）。
* 断电、强制关机、组策略强制锁屏，程序管不了。
* **本仓库的 CI 只在托管机上编译和跑测试，没有在真实一体机上验证过**，
  也没法在 CI 里验证远程会话。没跑过的机型和远程软件，不标注为"已保证兼容"。
* 界面内存没有实测数字：CI 的 Windows runner 没有可用桌面会话，界面起不来。
  在真实桌面上用任务管理器看一次才知道（见 `docs/architecture.md` § 7）。

---

## 九、构建

```bash
cargo test -p nd-core                                    # 纯逻辑，Linux 上也能跑
cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
cargo build --release -p nightdisplay                    # 产出 target/release/NightDisplay.exe
python tools/download_vdd.py --dest vendor/vdd           # 随包驱动（CI 会校验 SHA256）
```

工作区：

```text
crates/nd-core   纯逻辑：配置、时间计划、组合键、状态机、模式降级、显示路径选择、
                 会话记录、IPC 报文、诊断包、ZIP
crates/nd-win    Windows：显示配置、gamma、DDC/CI、黑幕、键鼠钩子、注册表、
                 计划任务、命名管道、提升助手、看门狗、VDD
crates/nd-gui    Tauri 2 界面：视图模型、命令层、托盘、窗口几何
crates/nd-cli    唯一的 exe（NightDisplay.exe）：参数解析 + 各动作 + 常驻进程
resources/       安装包资源（install_vdd.ps1、VDD 配置、驱动许可）
packaging/       Inno Setup 脚本与中文语言文件
legacy-python/   3.x 的 Python 实现，留作行为对照
docs/            架构说明、发布说明
```

CI（`.github/workflows/rust.yml`）：

| 作业 | 内容 |
|---|---|
| `windows` | `fmt --check` → `clippy -D warnings`（含 pedantic）→ `test --workspace` → release 构建 → 命令行冒烟（含诊断包） |
| `linux-logic` | 只跑 `nd-core` 的单元测试（时间计划、组合键、模式降级、显示路径选择） |
| `package` | Inno Setup 安装器 + 便携 zip（都带随包驱动），并**真的静默安装一遍再卸载** |
| `release` | 打 `v*` 标签时发布到 Releases，附 `SHA256SUMS.txt` |

架构、状态机、IPC 协议、看门狗契约和 lint 策略见 [`docs/architecture.md`](docs/architecture.md)。

---

## 十、更新记录

### 4.0.0

* Rust 重写，只保留 Windows。
* **默认模式改为虚拟屏**：一次 `SetDisplayConfig` 原子切换，虚拟屏常驻、实体屏摘掉桌面，
  远程采集表面不变 —— 不冻、不闪。
* 虚拟屏驱动**随包内置**，向导里 `pnputil` 静默安装，装完必须验证；
  `vendor/vdd` 在打包时校验 SHA256。
* 做不到无闪时自动降级到黑幕，并如实告知原因。
* 全新 Tauri 2 界面：深色侧边栏、倒计时环、自定义开关、托盘、记住窗口位置。
* 「运行日志 → 夜间会话」逐步骤显示耗时与结果；「导出诊断包」一键出脱敏 zip。
* 三条自启路径 + 看门狗 + 关机还原 + 崩溃恢复。
* 首次运行向导：一次 UAC、六步、每步真实结果、装完永不弹出、修复只重跑失败步骤。
* 所有特权动作收敛到一个提升助手，界面始终不以管理员身份运行。

3.x 的更新记录见 [`legacy-python/README.md`](legacy-python/README.md) 与历史提交。

---

## 许可

MIT，见 [LICENSE](LICENSE)。第三方许可见 [NOTICE.md](NOTICE.md)。
NightDisplay 是独立项目，与 UU 远程、ToDesk 没有隶属关系，也不使用它们的标识或素材。
