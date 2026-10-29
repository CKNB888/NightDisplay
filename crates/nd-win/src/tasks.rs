//! 每日计划任务：每个时间边界一个任务，跑 `--action reconcile`。
//!
//! 优先用 PowerShell 的 ScheduledTasks 模块（能设 `StartWhenAvailable`，
//! 睡眠错过的边界醒来后补跑）；PowerShell 不可用时退回 `schtasks`。
//! 任务名沿用 Python 版的 `NightDisplay_Night` / `NightDisplay_Day` / `NightDisplay_Txx`，
//! 升级和卸载都能认出来。

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

/// 一个要登记的任务。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskPlan {
    pub name: String,
    /// `HH:MM`，24 小时制。
    pub time: String,
}

/// 按配置算出任务清单：每个切换点一个。
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
    Ok(())
}

/// 看门狗任务是否已登记。
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
        assert_eq!(tasks[0].name, NIGHT_TASK);
        assert_eq!(tasks[1].name, DAY_TASK);
        assert_eq!(tasks[0].time, "20:00");
        assert_eq!(tasks[1].time, "10:30");
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
        assert_eq!(times, vec!["13:00", "14:00", "20:00", "10:30"]);
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
