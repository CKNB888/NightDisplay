//! 测试用的公共设施。
//!
//! `NIGHTDISPLAY_HOME` 是进程级环境变量，几个测试都要改它，并行跑会互相踩。
//! 所以统一用这里的互斥锁 + 独立临时目录，避免偶发的"配置读到了别人的目录"。

use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// 在一个干净的临时数据目录里跑测试闭包。
pub fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().expect("无法创建临时目录");
    std::env::set_var("NIGHTDISPLAY_HOME", dir.path());
    let out = f(dir.path());
    std::env::remove_var("NIGHTDISPLAY_HOME");
    out
}
