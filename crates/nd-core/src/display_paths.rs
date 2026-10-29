//! 显示路径集的选择逻辑（纯逻辑，不碰 Win32）。
//!
//! `nd-win::topology` 负责把 `QueryDisplayConfig` 的结果搬进来、把选出来的
//! 路径集交回 `SetDisplayConfig`；**选谁、改哪个标志位**这几条判断放在这里，
//! 因为它们是"会不会把远程画面搞没"的全部关键，必须能在 Linux CI 上断言。
//!
//! ## 为什么只有这几条判断这么重要
//!
//! 一次提交的路径集如果出现下面任何一种，用户看到的就是"远程黑屏时画面冻死"：
//!
//! * 活动 target 数为 0 —— 系统会把采集表面拆掉；
//! * 虚拟屏那条路径被重建（source id 或 target id 变了）—— 采集表面换了一块；
//! * 实体屏的 target 从路径集里被整个删掉 —— 设备消失，显示数量变化。
//!
//! 所以这里的函数都是"先算、后校验"：算出计划之后再断言一遍上述性质，
//! 断言不过就返回中文错误，调用方**不会**执行任何 `SetDisplayConfig`。
//!
//! ## 抽象
//!
//! [`PathView`] 只保留做判断需要的字段，不含适配器 LUID 和模式结构体 ——
//! 那些留在平台层，这里只关心"这条路径是什么、是不是活动的、有没有模式"。

use crate::error::user;
use crate::NdResult;

/// 一条显示路径的抽象视图（对应 `DISPLAYCONFIG_PATH_INFO` 的一个子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathView {
    /// `targetInfo.targetAvailable`：驱动说这个 target 插着东西。
    pub target_available: bool,
    /// 这条路径指向虚拟屏（输出技术是间接虚拟，或名字命中虚拟屏规则）。
    pub virtual_output: bool,
    /// 这条路径指向**本程序随包带的** VDD，而不是 UU/ToDesk 之类的第三方虚拟屏。
    pub bundled_vdd: bool,
    /// `flags & DISPLAYCONFIG_PATH_ACTIVE`。
    pub active: bool,
    /// `sourceInfo.modeInfoIdx` 有效（能拿到桌面模式）。
    pub has_source_mode: bool,
    /// `targetInfo.modeInfoIdx` 有效（能拿到信号模式）。
    pub has_target_mode: bool,
}

impl PathView {
    /// 构造函数，纯为测试和平台层转换写得短一点。
    #[must_use]
    pub fn new(virtual_output: bool) -> Self {
        Self {
            target_available: true,
            virtual_output,
            bundled_vdd: false,
            active: false,
            has_source_mode: true,
            has_target_mode: true,
        }
    }

    /// 链式设置"是本程序带的 VDD"。
    #[must_use]
    pub fn bundled(mut self) -> Self {
        self.bundled_vdd = true;
        self
    }

    /// 链式设置"当前活动"。
    #[must_use]
    pub fn active(mut self) -> Self {
        self.active = true;
        self
    }

    /// 链式设置"没有源模式"。
    #[must_use]
    pub fn without_source_mode(mut self) -> Self {
        self.has_source_mode = false;
        self
    }

    /// 链式设置"没有目标模式"。
    #[must_use]
    pub fn without_target_mode(mut self) -> Self {
        self.has_target_mode = false;
        self
    }
}

/// 挑虚拟 target 的规则：必须可用，必须是虚拟屏；
/// 优先随包 VDD，其次优先**已经活动的**（远程软件可能正抓着它），最后按下标。
fn pick_virtual(paths: &[PathView]) -> NdResult<usize> {
    paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.target_available && p.virtual_output)
        .max_by_key(|(i, p)| (p.bundled_vdd, p.active, usize::MAX - *i))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            user(
                "没有找到可用的虚拟屏（虚拟显示驱动）。请先在向导里装上随包的虚拟显示驱动，\
                 或运行 --action install-vdd 后重试",
            )
        })
}

fn count_active(paths: &[PathView]) -> usize {
    paths.iter().filter(|p| p.active).count()
}

/// 预热计划：把虚拟屏接进桌面（与实体屏共用同一个桌面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmPlan {
    /// 虚拟屏那条路径的下标。
    pub virtual_index: usize,
    /// 桌面来源（实体屏）的路径下标；`None` 表示已经预热过、不需要改。
    pub desktop_source: Option<usize>,
    /// 虚拟屏的模式直接照抄实体屏（一致 = 合成器不用重新初始化）。
    pub mode_matched: bool,
    /// 已经预热过，本次是幂等的空操作。
    pub already_armed: bool,
    /// 切换前活动 target 数。
    pub active_before: usize,
    /// 计划提交后的活动 target 数。
    pub active_after: usize,
}

/// 计算预热计划。已经预热过时返回幂等的空计划，不报错。
pub fn plan_arm(paths: &[PathView]) -> NdResult<ArmPlan> {
    let virtual_index = pick_virtual(paths)?;
    let active_before = count_active(paths);

    if paths[virtual_index].active {
        return Ok(ArmPlan {
            virtual_index,
            desktop_source: None,
            mode_matched: true,
            already_armed: true,
            active_before,
            active_after: active_before,
        });
    }

    // 桌面来源：一块活动的实体屏，并且拿得到源模式（否则复制过去也没有桌面可接）。
    let desktop_source = paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.active && !p.virtual_output && p.has_source_mode)
        .max_by_key(|(_, p)| (p.has_target_mode, !p.bundled_vdd))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            user("当前没有活动的实体屏，虚拟屏没有可复制的桌面来源（请先确认显示器已连接并处于活动状态）")
        })?;

    let plan = ArmPlan {
        virtual_index,
        desktop_source: Some(desktop_source),
        mode_matched: paths[desktop_source].has_target_mode,
        already_armed: false,
        active_before,
        active_after: active_before + 1,
    };
    // 预热也必须保证提交后至少有一个活动 target。
    if plan.active_after == 0 {
        return Err(user("预热计划会让机器没有活动显示，已中止"));
    }
    Ok(plan)
}

/// 入夜计划：摘掉实体屏的桌面，虚拟屏继续接着原来那块桌面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachPlan {
    /// 虚拟屏那条路径的下标（**必须**保持活动，且身份不变）。
    pub virtual_index: usize,
    /// 被摘掉桌面的实体屏路径下标。
    pub deactivated: Vec<usize>,
    /// 切换前活动 target 数。
    pub active_before: usize,
    /// 计划提交后的活动 target 数（永远 ≥ 1）。
    pub active_after: usize,
}

/// 计算入夜计划。任何"会让机器没有显示"或"虚拟屏还没预热"的情况都返回错误。
pub fn plan_detach(paths: &[PathView]) -> NdResult<DetachPlan> {
    let virtual_index = pick_virtual(paths)?;
    if !paths[virtual_index].active {
        return Err(user(
            "虚拟屏还没有接入桌面（未预热）。请先运行一次预热（重启程序会自动预热）",
        ));
    }
    let active_before = count_active(paths);
    let deactivated: Vec<usize> = paths
        .iter()
        .enumerate()
        .filter(|(i, p)| *i != virtual_index && p.active && !p.virtual_output)
        .map(|(i, _)| i)
        .collect();
    if deactivated.is_empty() {
        return Err(user(
            "虚拟屏已经接管，没有需要摘掉的实体屏（当前已经是夜间状态）",
        ));
    }
    let active_after = active_before - deactivated.len();
    let plan = DetachPlan {
        virtual_index,
        deactivated,
        active_before,
        active_after,
    };
    check_detached(&plan)?;
    Ok(plan)
}

/// 断言入夜计划是安全的。单独拿出来，供平台层在执行前再核一遍。
pub fn check_detached(plan: &DetachPlan) -> NdResult<()> {
    if plan.active_after == 0 {
        return Err(user(
            "切换后没有任何活动显示，已中止（不会让机器变成无显示状态）",
        ));
    }
    if plan.deactivated.contains(&plan.virtual_index) {
        return Err(user("切换计划会摘掉虚拟屏自己，已中止"));
    }
    Ok(())
}

/// 切换后核对：系统真的按计划做了吗。返回 `Ok(())` 才算成功。
///
/// * `after` 是重新 `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)` 的结果；
/// * `virtual_index` 是入夜计划里那条虚拟屏路径在**新**结果里的下标 ——
///   平台层按 target 标识重新找，不靠下标猜；
/// * `identity_kept` 表示虚拟屏的 target 标识没变（采集表面没被重建）。
pub fn verify_detached(
    after: &[PathView],
    virtual_index: usize,
    identity_kept: bool,
) -> NdResult<()> {
    if after.is_empty() {
        return Err(user("提交后系统报告没有任何活动显示，请立刻按 Win+P 恢复"));
    }
    if let Some(p) = after.iter().find(|p| !p.virtual_output) {
        let n = after.iter().filter(|p| !p.virtual_output).count();
        return Err(user(format!(
            "提交成功但结果不对：活动显示里还有实体屏（{n} 块，例如 {}）",
            if p.active {
                "一块活动的实体屏"
            } else {
                "实体屏"
            }
        )));
    }
    if !after.get(virtual_index).is_some_and(|p| p.virtual_output) {
        return Err(user("提交成功但活动显示里找不到虚拟屏"));
    }
    if !identity_kept {
        return Err(user("提交成功但虚拟屏的 target 变了，采集表面可能已经重建"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical() -> PathView {
        PathView::new(false).active()
    }

    fn vdd() -> PathView {
        PathView::new(true).bundled()
    }

    #[test]
    fn arm_takes_the_bundled_vdd_and_shares_the_desktop() {
        let paths = vec![physical(), vdd()];
        let plan = plan_arm(&paths).unwrap();
        assert_eq!(plan.virtual_index, 1);
        assert_eq!(plan.desktop_source, Some(0));
        assert!(plan.mode_matched);
        assert!(!plan.already_armed);
        assert_eq!(plan.active_before, 1);
        assert_eq!(plan.active_after, 2);
    }

    #[test]
    fn arm_prefers_bundled_over_third_party_virtual() {
        // UU 的虚拟屏排前面，但带的是自家驱动，必须选后面那个。
        let paths = vec![PathView::new(true).active(), vdd(), physical()];
        assert_eq!(plan_arm(&paths).unwrap().virtual_index, 1);
    }

    #[test]
    fn arm_is_idempotent_once_armed() {
        let paths = vec![physical(), vdd().active()];
        let plan = plan_arm(&paths).unwrap();
        assert!(plan.already_armed);
        assert_eq!(plan.desktop_source, None);
        assert_eq!(plan.active_after, plan.active_before);
    }

    #[test]
    fn arm_requires_a_physical_source() {
        // 只有虚拟屏，没有任何活动实体屏：不能凭空造一块桌面出来。
        let paths = vec![vdd()];
        let err = plan_arm(&paths).unwrap_err().to_string();
        assert!(err.contains("实体屏"), "{err}");
    }

    #[test]
    fn arm_reports_a_physical_without_target_mode() {
        let paths = vec![physical().without_target_mode(), vdd()];
        let plan = plan_arm(&paths).unwrap();
        assert!(!plan.mode_matched, "照抄不到信号模式时必须如实报告");
    }

    #[test]
    fn arm_fails_without_any_virtual_target() {
        let paths = vec![physical()];
        let err = plan_arm(&paths).unwrap_err().to_string();
        assert!(err.contains("虚拟屏"), "{err}");
    }

    #[test]
    fn detach_clears_only_physical_paths() {
        let paths = vec![physical(), physical(), vdd().active()];
        let plan = plan_detach(&paths).unwrap();
        assert_eq!(plan.virtual_index, 2);
        assert_eq!(plan.deactivated, vec![0, 1]);
        assert_eq!(plan.active_before, 3);
        assert_eq!(plan.active_after, 1, "只剩虚拟屏，但绝不是 0");
    }

    #[test]
    fn detach_keeps_other_virtual_targets_active() {
        // 第三方虚拟屏也活动着：它不是实体屏，不该被摘掉（别人可能在用）。
        let paths = vec![physical(), PathView::new(true).active(), vdd().active()];
        let plan = plan_detach(&paths).unwrap();
        assert_eq!(plan.deactivated, vec![0]);
        assert_eq!(plan.active_after, 2);
    }

    #[test]
    fn detach_requires_prewarm() {
        let paths = vec![physical(), vdd()];
        let err = plan_detach(&paths).unwrap_err().to_string();
        assert!(err.contains("预热"), "{err}");
    }

    #[test]
    fn detach_refuses_when_already_detached() {
        let paths = vec![vdd().active()];
        let err = plan_detach(&paths).unwrap_err().to_string();
        assert!(err.contains("已经是夜间"), "{err}");
    }

    #[test]
    fn detach_never_leaves_zero_targets() {
        // 唯一一块"实体屏"其实是虚拟屏时，没有东西可摘 —— 必须报错而不是提交空集。
        let paths = vec![PathView::new(true).active()];
        assert!(plan_detach(&paths).is_err());
    }

    #[test]
    fn check_detached_rejects_a_zero_target_plan() {
        let plan = DetachPlan {
            virtual_index: 0,
            deactivated: vec![1],
            active_before: 1,
            active_after: 0,
        };
        assert!(check_detached(&plan).is_err());
    }

    #[test]
    fn check_detached_rejects_deactivating_the_virtual_screen() {
        let plan = DetachPlan {
            virtual_index: 1,
            deactivated: vec![0, 1],
            active_before: 2,
            active_after: 0,
        };
        assert!(check_detached(&plan).is_err());
    }

    #[test]
    fn verify_accepts_a_virtual_only_desktop() {
        let after = vec![vdd().active()];
        assert!(verify_detached(&after, 0, true).is_ok());
    }

    #[test]
    fn verify_rejects_a_physical_still_on_the_desktop() {
        let after = vec![vdd().active(), physical()];
        let err = verify_detached(&after, 0, true).unwrap_err().to_string();
        assert!(err.contains("实体屏"), "{err}");
    }

    #[test]
    fn verify_rejects_a_rebuilt_virtual_surface() {
        let after = vec![vdd().active()];
        let err = verify_detached(&after, 0, false).unwrap_err().to_string();
        assert!(err.contains("target"), "{err}");
    }

    #[test]
    fn verify_rejects_an_empty_active_set() {
        assert!(verify_detached(&[], 0, true).is_err());
    }

    /// 性质测试：任意"一块实体屏 + 一块虚拟屏"的组合，入夜后活动数恒为 1，
    /// 且实体屏全部不在活动集合里。
    #[test]
    fn any_physical_count_detaches_to_exactly_one_target() {
        for physicals in 1..=4usize {
            let mut paths: Vec<PathView> = (0..physicals).map(|_| physical()).collect();
            paths.push(vdd().active());
            let plan = plan_detach(&paths).unwrap();
            assert_eq!(plan.active_after, 1, "physicals = {physicals}");
            assert_eq!(plan.deactivated.len(), physicals);
            // 模拟提交后的活动集合。
            let after: Vec<PathView> = vec![vdd().active()];
            assert!(verify_detached(&after, 0, true).is_ok());
        }
    }

    /// 性质测试：预热计划的活动数恒为 `活动数 + 1`，且虚拟屏一定在其中。
    #[test]
    fn arm_always_adds_exactly_one_active_target() {
        for physicals in 1..=4usize {
            let mut paths: Vec<PathView> = (0..physicals).map(|_| physical()).collect();
            paths.push(vdd());
            let plan = plan_arm(&paths).unwrap();
            assert_eq!(plan.active_after, plan.active_before + 1);
            assert_eq!(plan.virtual_index, physicals);
        }
    }
}
