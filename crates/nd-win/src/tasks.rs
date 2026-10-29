//! 计划任务：每日边界任务 + 开机/登录自启 + 看门狗。
//!
//! ## 为什么要三条自启路径
//!
//! 断电、蓝屏、强制关机之后，「登录时触发」的任务不一定会补跑，
//! 而「开机时触发」的任务会。所以自启是这样铺的：
//!
//! | 路径 | 触发 | 身份 | 作用 |
//! |---|---|---|---|
//! | HKCU Run | 用户登录 | 当前用户（普通权限） | 最普通的一条，被安全软件拦掉的概率最低 |
//! | `NightDisplay_Logon` | 登录 | 当前用户 | 带 `StartWhenAvailable`，错过就补 |
//! | `NightDisplay_Boot` | 开机（延迟 30 秒） | SYSTEM + 最高权限 | 断电后也会跑 |
//!
//! 三条路径都只是把同一个 exe 拉起来，单实例锁（`daemon.lock`）保证只有一个
//! 真的在跑，其余几个立刻退出 —— 重复触发是安全的。
//!
//! 每日边界任务（`NightDisplay_Night` / `_Day` / `_Txx`）沿用旧版命名，
//! 升级和卸载都能认出来。优先用 PowerShell 的 `ScheduledTasks` 模块
//! （能设 `StartWhenAvailable`，睡眠错过的边界醒来后补跑）；不可用时退回 `schtasks`。

#![cfg(windows)]

use std::path::Path;

use anyhow::Result;
use nd_core::config::Config;

use crate::sysutil;

/// 前两个边界用固定名字，多余的边界用序号。和旧版保持一致。
pub const NIGHT_TASK: &str = "NightDisplay_Night";
pub const DAY_TASK: &str = "NightDisplay_Day";
/// 看门狗任务：登录后常驻，主进程死掉几秒内恢复屏幕。
pub const WATCHDOG_TASK: &str = "NightDisplay_Watchdog";
/// 登录时拉起常驻进程（带 `StartWhenAvailable`）。
pub const LOGON_TASK: &str = "NightDisplay_Logon";
/// 开机时拉起常驻进程，SYSTEM + 最高权限 + 延迟启动。
pub const BOOT_TASK: &str = "NightDisplay_Boot";
/// 开机任务延迟多少秒，避开开机时的服务风暴。
pub const BOOT_DELAY: &str = "PT30S";
/// 看门狗任务在失败后重启的间隔（ISO 8601 时长）。
pub const WATCHDOG_RESTART: &str = "PT1M";

const TASK_SCRIPT: &str = r#"
$ErrorActionPreference = "Stop"
$payload = $env:NIGHTDISPLAY_TASKS | ConvertFrom-Json
$action = New-ScheduledTaskAction -Execute $payload.exe -Argument $payload.args
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew
$account = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal = New-ScheduledTaskPrincipal -UserId $account -LogonType Interactive -RunLevel Limited
foreach ($task in $payload.tasks) {
  $when = [datetime]::ParseExact($task.time, "HH:mm", [System.Globalization.CultureInfo]::InvariantCulture)
  $trigger = New-ScheduledTaskTrigger -Daily -At $when
  Register-ScheduledTask -TaskName $task.name -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null
}
"#;

/// 注册两条自启任务的脚本（一次 PowerShell 调用，两条一起登记）。
///
/// * `NightDisplay_Logon`：当前用户、登录触发、`StartWhenAvailable`；
/// * `NightDisplay_Boot`：SYSTEM、开机触发、延迟 30 秒、最高权限、
///   失败后每 1 分钟重启一次（最多 3 次）。
///
/// 两条都跑 `--action reconcile`，重复触发由单实例锁挡掉。
const AUTOSTART_SCRIPT: &str = r#"
$ErrorActionPreference = "Stop"
$payload = $env:NIGHTDISPLAY_AUTOSTART | ConvertFrom-Json
$exe = $payload.exe
$args = $payload.args

# 登录任务：当前用户，普通权限即可（它只负责把常驻进程拉起来）。
$action = New-ScheduledTaskAction -Execute $exe -Argument $args
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
$account = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal = New-ScheduledTaskPrincipal -UserId $account -LogonType Interactive -RunLevel Limited
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $account
Register-ScheduledTask -TaskName $payload.logon -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null

# 开机任务：SYSTEM + 最高权限 + 延迟，断电重启后也能跑起来。
$bootAction = New-ScheduledTaskAction -Execute $exe -Argument $args
$bootSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
$bootPrincipal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$bootTrigger = New-ScheduledTaskTrigger -AtStartup
$bootTrigger.Delay = $payload.delay
Register-ScheduledTask -TaskName $payload.boot -Action $bootAction -Trigger $bootTrigger -Settings $bootSettings -Principal $bootPrincipal -Force | Out-Null
"#;

/// 一个要登记的任务。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskPlan {
    pub name: String,
    /// `HH:MM`，24 小时制。
    pub time: String,
}

/// 按配置算出任务清单：每个切换点一个。
#[must_use]
pub fn plan(cfg: &Config) -> Vec<TaskPlan> {
    cfg.schedule()
        .boundaries()
        .into_iter()
        .enumerate()
        .map(|(i, m)| TaskPlan {
            name: match i {
                0 => NIGHT_TASK.to_string(),
                1 => DAY_TASK.to_string(),
                n => format!("NightDisplay_T{n:02}"),
            },
            time: m.to_hm(),
        })
        .collect()
}

/// 登记全部每日任务。返回实际登记的任务名。
pub fn install(cfg: &Config, home: &Path) -> Result<Vec<String>> {
    let plan = plan(cfg);
    if plan.is_empty() {
        return Err(nd_core::error::user("没有可用的时间段，无法登记计划任务"));
    }
    register_plan(&plan, home)?;
    Ok(plan.into_iter().map(|t| t.name).collect())
}

/// 直接按时间点字符串登记（提升助手用：它不读用户配置）。
pub fn install_boundaries(boundaries: &[String], home: &Path) -> Result<Vec<String>> {
    if boundaries.is_empty() {
        return Err(nd_core::error::user("没有可用的时间段，无法登记计划任务"));
    }
    let plan: Vec<TaskPlan> = boundaries
        .iter()
        .enumerate()
        .map(|(i, time)| TaskPlan {
            name: match i {
                0 => NIGHT_TASK.to_string(),
                1 => DAY_TASK.to_string(),
                n => format!("NightDisplay_T{n:02}"),
            },
            time: time.clone(),
        })
        .collect();
    register_plan(&plan, home)?;
    Ok(plan.into_iter().map(|t| t.name).collect())
}

/// 登记一份算好的任务清单，并回读校验。
fn register_plan(plan: &[TaskPlan], home: &Path) -> Result<()> {
    // 先清掉旧的，避免时间段改少以后留下多余任务。
    let _ = remove_all();

    if let Err(e) = register_powershell(plan, home) {
        tracing::warn!(error = %e, "ScheduledTasks 模块不可用，改用 schtasks");
        // PowerShell 可能已经登记了一部分同名任务，先清干净再交给 schtasks。
        let _ = remove_all();
        register_schtasks(plan, home)?;
    }
    // 回读校验：任务必须真的在。
    let existing = query_names();
    let missing: Vec<&str> = plan
        .iter()
        .map(|t| t.name.as_str())
        .filter(|n| !existing.iter().any(|e| e == n))
        .collect();
    if !missing.is_empty() {
        return Err(nd_core::error::user(format!(
            "计划任务登记后没有查到：{}（可能被安全软件拦下了）",
            missing.join("、")
        )));
    }
    tracing::info!(
        tasks = %plan.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join("、"),
        "已登记每日计划任务"
    );
    Ok(())
}

fn register_powershell(plan: &[TaskPlan], home: &Path) -> Result<()> {
    let args = task_args(home);
    let payload = serde_json::json!({
        "exe": sysutil::exe_path().display().to_string(),
        "args": args,
        "tasks": plan,
    });
    let out =
        sysutil::run_powershell(TASK_SCRIPT, &[("NIGHTDISPLAY_TASKS", &payload.to_string())])?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(nd_core::error::user(format!(
            "PowerShell 登记计划任务失败：{}",
            err.trim().lines().last().unwrap_or("未知原因")
        )));
    }
    Ok(())
}

fn register_schtasks(plan: &[TaskPlan], home: &Path) -> Result<()> {
    let tr = sysutil::command_line(&sysutil::exe_path(), &task_args(home));
    let mut failed = Vec::new();
    for task in plan {
        let out = sysutil::run_hidden(
            std::ffi::OsStr::new("schtasks"),
            &[
                "/Create", "/F", "/TN", &task.name, "/SC", "DAILY", "/ST", &task.time, "/TR", &tr,
                "/IT",
            ],
            "schtasks",
        )?;
        if !out.status.success() {
            failed.push(format!("{}（{}）", task.name, task.time));
        }
    }
    if !failed.is_empty() {
        return Err(nd_core::error::user(format!(
            "登记计划任务失败：{}",
            failed.join("、")
        )));
    }
    Ok(())
}

fn task_args(home: &Path) -> Vec<String> {
    sysutil::self_args(home, &["--action", "reconcile"])
}

/// 当前系统里存在的本程序任务名。
#[must_use]
pub fn query_names() -> Vec<String> {
    let Ok(out) = sysutil::run_hidden(
        std::ffi::OsStr::new("schtasks"),
        &["/Query", "/FO", "CSV", "/NH"],
        "schtasks",
    ) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('"') {
            let name: String = rest.chars().take_while(|c| *c != '"').collect();
            let name = name.trim_start_matches('\\').to_string();
            if name.starts_with("NightDisplay_") && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// 删掉本程序登记的全部任务（数量不限）。
pub fn remove_all() -> Result<usize> {
    let mut removed = 0;
    for name in query_names() {
        let out = sysutil::run_hidden(
            std::ffi::OsStr::new("schtasks"),
            &["/Delete", "/F", "/TN", &name],
            "schtasks",
        )?;
        if out.status.success() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "已删除计划任务");
    }
    Ok(removed)
}

/// 登记两条自启任务（登录 + 开机）。**幂等**：重复调用只会覆盖同名任务。
pub fn install_autostart(home: &Path) -> Result<()> {
    let payload = serde_json::json!({
        "exe": sysutil::exe_path().display().to_string(),
        "args": sysutil::command_line(Path::new(""), &task_args(home)),
        "logon": LOGON_TASK,
        "boot": BOOT_TASK,
        "delay": BOOT_DELAY,
    });
    let out = sysutil::run_powershell(
        AUTOSTART_SCRIPT,
        &[("NIGHTDISPLAY_AUTOSTART", &payload.to_string())],
    )?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let reason = err.trim().lines().last().unwrap_or("未知原因").to_string();
        // 开机任务（SYSTEM）在个别策略下会被拒；登录任务能成就还能用，如实说明。
        let existing = query_names();
        let logon_ok = existing.iter().any(|n| n == LOGON_TASK);
        let boot_ok = existing.iter().any(|n| n == BOOT_TASK);
        if logon_ok {
            return Err(nd_core::error::user(format!(
                "登录自启已登记，但开机自启（SYSTEM）失败：{reason}。\
                 断电重启后需要等用户登录才会恢复；如果设置了自动登录则没有影响"
            )));
        }
        let _ = boot_ok;
        return Err(nd_core::error::user(format!(
            "登记自启任务失败：{reason}（可能被组策略或安全软件拦下了）"
        )));
    }
    // 回读校验：两条都必须真的在。
    let existing = query_names();
    let mut missing = Vec::new();
    for name in [LOGON_TASK, BOOT_TASK] {
        if !existing.iter().any(|n| n == name) {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(nd_core::error::user(format!(
            "自启任务登记后没有查到：{}（可能被安全软件拦下了）",
            missing.join("、")
        )));
    }
    tracing::info!("已登记登录 + 开机自启任务");
    Ok(())
}

/// 删除两条自启任务。不存在也算成功。
pub fn remove_autostart() -> Result<()> {
    let existing = query_names();
    for name in [LOGON_TASK, BOOT_TASK] {
        if !existing.iter().any(|n| n == name) {
            continue;
        }
        let out = sysutil::run_hidden(
            std::ffi::OsStr::new("schtasks"),
            &["/Delete", "/F", "/TN", name],
            "schtasks",
        )?;
        if !out.status.success() {
            tracing::warn!(task = name, "删除自启任务失败");
        }
    }
    Ok(())
}

/// 两条自启任务是否都在。
#[must_use]
pub fn autostart_tasks_installed() -> (bool, bool) {
    let names = query_names();
    (
        names.iter().any(|n| n == LOGON_TASK),
        names.iter().any(|n| n == BOOT_TASK),
    )
}

/// 回读三条自启路径 + 看门狗 + 每日任务的**真实**状态。
///
/// 设置页显示的必须是这个，而不是用户勾的那个开关：任务可能被安全软件删了，
/// Run 项可能被「启动」页禁用了，这些都要如实反映出来。
#[must_use]
pub fn registration() -> nd_core::status::Registration {
    let (task_logon, task_boot) = autostart_tasks_installed();
    let names = query_names();
    let schedule_tasks = names
        .iter()
        .filter(|n| {
            n.as_str() != LOGON_TASK && n.as_str() != BOOT_TASK && n.as_str() != WATCHDOG_TASK
        })
        .count();
    let home = nd_core::paths::home_dir();
    let run_key = crate::autostart::is_enabled();
    let mut note = crate::autostart::diagnose(&home);
    if note == "已注册" {
        note = String::new();
    }
    nd_core::status::Registration {
        run_key,
        task_logon,
        task_boot,
        watchdog_task: names.iter().any(|n| n == WATCHDOG_TASK),
        schedule_tasks,
        run_key_note: note,
    }
}

/// 登记看门狗任务：登录后拉起，主进程退出时恢复屏幕。
///
/// 用 `ONLOGON` 触发；它本身是常驻的轻量进程，只监听父进程 PID。
pub fn install_watchdog(home: &Path) -> Result<()> {
    let args = sysutil::command_line(
        &sysutil::exe_path(),
        &sysutil::self_args(home, &["--action", "watchdog", "--autostart"]),
    );
    let out = sysutil::run_hidden(
        std::ffi::OsStr::new("schtasks"),
        &[
            "/Create",
            "/F",
            "/TN",
            WATCHDOG_TASK,
            "/SC",
            "ONLOGON",
            "/TR",
            &args,
            "/IT",
        ],
        "schtasks",
    )?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(nd_core::error::user(format!(
            "登记看门狗任务失败：{}",
            err.trim().lines().last().unwrap_or("未知原因")
        )));
    }
    // 补上"失败后自动重启"：schtasks 设不了，用 PowerShell 改一遍设置。
    let _ = enable_restart_on_failure(WATCHDOG_TASK, WATCHDOG_RESTART);
    Ok(())
}

/// 给已有任务加上"失败后每隔 interval 重启一次（最多 3 次）"。
fn enable_restart_on_failure(task: &str, interval: &str) -> Result<()> {
    let script = format!(
        r#"$ErrorActionPreference = "Stop"
$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval ([TimeSpan]::Parse("{interval}"))
Set-ScheduledTask -TaskName "{task}" -Settings $s | Out-Null
"#
    );
    let out = sysutil::run_powershell(&script, &[])?;
    if !out.status.success() {
        tracing::warn!(task, "看门狗任务没能设置失败重启，主进程崩溃时不会自动重拉");
    }
    Ok(())
}

/// 看门狗任务是否已登记。
#[must_use]
pub fn watchdog_installed() -> bool {
    query_names().iter().any(|n| n == WATCHDOG_TASK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_core::config::Config;

    #[test]
    fn plan_has_two_tasks_for_one_range() {
        let cfg = Config::default();
        let tasks = plan(&cfg);
        assert_eq!(tasks.len(), 2);
        // 边界按一天内的先后排序，所以跨午夜的主时段是 10:30 在前。
        // 任务名只是标识：每个任务都跑 `--action reconcile`，按当前时间重新判断。
        assert_eq!(tasks[0].name, NIGHT_TASK);
        assert_eq!(tasks[1].name, DAY_TASK);
        assert_eq!(tasks[0].time, "10:30");
        assert_eq!(tasks[1].time, "20:00");
    }

    #[test]
    fn extra_boundaries_get_numbered_names() {
        let mut cfg = Config::default();
        cfg.ranges.push(nd_core::config::RangeDef(
            nd_core::schedule::NightRange::new(
                "13:00".parse::<nd_core::schedule::MinuteOfDay>().unwrap(),
                "14:00".parse::<nd_core::schedule::MinuteOfDay>().unwrap(),
            )
            .unwrap(),
        ));
        let tasks = plan(&cfg);
        assert_eq!(tasks.len(), 4);
        assert!(tasks.iter().any(|t| t.name == "NightDisplay_T02"));
        // 时间必须单调递增，任务名按这个顺序分配。
        let times: Vec<&str> = tasks.iter().map(|t| t.time.as_str()).collect();
        assert_eq!(times, vec!["10:30", "13:00", "14:00", "20:00"]);
    }

    #[test]
    fn task_args_use_reconcile() {
        let args = task_args(Path::new(r"C:\nd"));
        assert_eq!(args[0], "--home");
        assert_eq!(args[2], "--action");
        assert_eq!(args[3], "reconcile");
    }

    #[test]
    fn plan_serializes_for_powershell() {
        let json = serde_json::to_string(&plan(&Config::default())).unwrap();
        assert!(json.contains("NightDisplay_Night"));
        assert!(json.contains("\"time\":\"20:00\""));
    }
}
