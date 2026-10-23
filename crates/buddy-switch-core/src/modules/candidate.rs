//! Isolated identity of the local integration preview. Upstream installations
//! and their data are never migrated or adopted implicitly.
//!
//! ## 外部客户端目录的**联接**例外（2026-10-21）
//!
//! Qoder / WorkBuddy 客户端是**外部应用**，其数据（会话记录、登录态、机器身份）
//! 永远在真实用户主目录下。预览隔离只隔离**本应用自身**的数据；若把外部客户端
//! 也「搬进」预览家目录，客户端会话记录/用量/切换等功能的语义就断了
//! （真机症状：会话记录页提示「没有找到客户端会话记录」，而客户端明明有数据）。
//! 故启动时把客户端目录以**目录联接**并入预览家目录：读写直达真实客户端，
//! 生产语义一致；目标不存在（客户端未装/未用）时跳过，视图如实显示为空。
//! 2026-10-22 起默认不联接，需显式开启 BUDDY_SWITCH_PREVIEW_LINK_CLIENT_DIRS=1。
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const IS_FUSION_PREVIEW: bool = true;

/// 控制是否把外部客户端目录联接进整合预览家目录的环境变量名。
pub const PREVIEW_LINK_CLIENT_DIRS_ENV: &str = "BUDDY_SWITCH_PREVIEW_LINK_CLIENT_DIRS";

/// 需要联接进预览家目录的**外部客户端**数据目录（相对真实用户主目录）。
const CLIENT_LINK_DIRS: &[&str] = &[".qoder-cn", ".qoder", ".workbuddy", ".workbuddy-ai"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchParseOutcome {
    Off,
    On,
    Unrecognized,
}

impl SwitchParseOutcome {
    #[inline]
    pub(crate) fn is_enabled(self) -> bool {
        matches!(self, SwitchParseOutcome::On)
    }
}

/// 解析外部客户端目录联接开关。
///
/// 纯函数，入参为原始环境变量文本：
/// - `None` / 空白 / "0" / "false" / "off" / "no"（不区分大小写、先 trim）⇒ `Off`（关）；
/// - "1" / "true" / "on" / "yes"（不区分大小写、先 trim）⇒ `On`（开）；
/// - 其他非空字符串 ⇒ `Unrecognized`（无法识别，后续作为关处理并发出一次性告警）。
pub(crate) fn parse_preview_link_client_dirs_value(raw: Option<&str>) -> SwitchParseOutcome {
    let Some(val) = raw else {
        return SwitchParseOutcome::Off;
    };
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return SwitchParseOutcome::Off;
    }
    if trimmed.eq_ignore_ascii_case("1")
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("on")
        || trimmed.eq_ignore_ascii_case("yes")
    {
        SwitchParseOutcome::On
    } else if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("no")
    {
        SwitchParseOutcome::Off
    } else {
        SwitchParseOutcome::Unrecognized
    }
}

static UNRECOGNIZED_ENV_WARNED: AtomicBool = AtomicBool::new(false);
static PREVIEW_LINK_STATE: Mutex<Option<bool>> = Mutex::new(None);

fn resolve_preview_link_client_dirs_from_env() -> bool {
    let raw = std::env::var(PREVIEW_LINK_CLIENT_DIRS_ENV).ok();
    let parsed = parse_preview_link_client_dirs_value(raw.as_deref());
    if parsed == SwitchParseOutcome::Unrecognized {
        if let Some(val) = raw {
            if !UNRECOGNIZED_ENV_WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[preview] 环境变量 {PREVIEW_LINK_CLIENT_DIRS_ENV}={val:?} 无法识别，已视为关（默认不联接客户端目录）。"
                );
            }
        }
    }
    parsed.is_enabled()
}

/// 查询本次进程整合预览版是否开启了外部客户端目录联接。
///
/// 单例初始化，多次调用结果一致，与 `initialize()` 的决策同源。
pub fn preview_client_dirs_link_enabled() -> bool {
    let mut lock = PREVIEW_LINK_STATE.lock().unwrap();
    if let Some(enabled) = *lock {
        return enabled;
    }
    let enabled = resolve_preview_link_client_dirs_from_env();
    *lock = Some(enabled);
    enabled
}

#[cfg(test)]
pub(crate) fn reset_preview_link_state_for_test() {
    let mut lock = PREVIEW_LINK_STATE.lock().unwrap();
    *lock = None;
    UNRECOGNIZED_ENV_WARNED.store(false, Ordering::Relaxed);
}

fn allowed_root(root: &Path, real_home: &Path) -> bool {
    root.is_absolute() && root != real_home && root != real_home.join(".buddy-switch")
}

#[cfg(windows)]
fn make_dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
    // 目录联接（junction）不需要特权；`symlink_dir` 需要特权，仅作回退。
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(windows))]
fn make_dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 把外部客户端数据目录联接进预览家目录。
///
/// - 预览侧同名路径**已存在**（含悬挂联接）⇒ 跳过，绝不覆盖；
/// - 真实客户端目录不存在 ⇒ 跳过（功能如实显示为空，不凭空造目录）；
/// - 单个联接失败不阻断启动（对应视图会如实显示为空/报错）。
fn link_external_client_dirs(root: &Path, real_home: &Path) {
    for dir in CLIENT_LINK_DIRS {
        let link = root.join(dir);
        if link.symlink_metadata().is_ok() {
            continue;
        }
        let target = real_home.join(dir);
        if !target.is_dir() {
            continue;
        }
        if let Err(error) = make_dir_link(&target, &link) {
            eprintln!("[preview] 客户端目录联接失败（{dir}）：{error}");
        }
    }
}

/// Call before creating any runtime, reading configuration, or spawning threads.
pub fn initialize() -> Result<PathBuf, String> {
    let real_home = dirs::home_dir().ok_or("无法定位用户主目录")?;
    let root = match std::env::var(super::config::BUDDY_SWITCH_HOME_ENV) {
        Ok(value) if !value.trim().is_empty() => {
            let root = PathBuf::from(value);
            if !root.is_dir() {
                return Err("整合预览版数据目录必须是已存在的绝对目录".into());
            }
            root
        }
        _ => real_home.join(".buddy-qoder-switch-preview"),
    };
    if !allowed_root(&root, &real_home) {
        return Err("整合预览版拒绝使用现有 BuddySwitch 的用户目录".into());
    }
    std::fs::create_dir_all(&root).map_err(|_| "无法创建整合预览版独立目录")?;
    let canonical = root.canonicalize().map_err(|_| "无法解析整合预览版目录")?;
    let canonical_home = real_home.canonicalize().map_err(|_| "无法解析用户主目录")?;
    if !allowed_root(&canonical, &canonical_home) {
        return Err("整合预览版目录指向现有 BuddySwitch 数据，已拒绝启动".into());
    }
    std::env::set_var(super::config::BUDDY_SWITCH_HOME_ENV, &canonical);
    if preview_client_dirs_link_enabled() {
        // 用**未 canonical 化**的路径建联接（`\\?\` 前缀会让 mklink 不认）。
        link_external_client_dirs(&root, &real_home);
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PreviewTestEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_link_env: Option<std::ffi::OsString>,
        prev_home_env: Option<std::ffi::OsString>,
    }

    impl PreviewTestEnvGuard {
        fn new(link_val: Option<&str>, home_dir: Option<&Path>) -> Self {
            let lock = crate::modules::config::env_lock();
            let prev_link_env = std::env::var_os(PREVIEW_LINK_CLIENT_DIRS_ENV);
            let prev_home_env = std::env::var_os(crate::modules::config::BUDDY_SWITCH_HOME_ENV);

            match link_val {
                Some(v) => std::env::set_var(PREVIEW_LINK_CLIENT_DIRS_ENV, v),
                None => std::env::remove_var(PREVIEW_LINK_CLIENT_DIRS_ENV),
            }
            if let Some(h) = home_dir {
                std::env::set_var(crate::modules::config::BUDDY_SWITCH_HOME_ENV, h);
            }
            reset_preview_link_state_for_test();

            Self {
                _lock: lock,
                prev_link_env,
                prev_home_env,
            }
        }
    }

    impl Drop for PreviewTestEnvGuard {
        fn drop(&mut self) {
            match self.prev_link_env.take() {
                Some(v) => std::env::set_var(PREVIEW_LINK_CLIENT_DIRS_ENV, v),
                None => std::env::remove_var(PREVIEW_LINK_CLIENT_DIRS_ENV),
            }
            match self.prev_home_env.take() {
                Some(v) => std::env::set_var(crate::modules::config::BUDDY_SWITCH_HOME_ENV, v),
                None => std::env::remove_var(crate::modules::config::BUDDY_SWITCH_HOME_ENV),
            }
            reset_preview_link_state_for_test();
        }
    }

    #[test]
    fn refuses_real_home_and_existing_store() {
        let home = std::env::temp_dir().join("fusion-fixture-home");
        assert!(!allowed_root(&home, &home));
        assert!(!allowed_root(&home.join(".buddy-switch"), &home));
        assert!(!allowed_root(Path::new("relative"), &home));
        assert!(allowed_root(&home.join("preview"), &home));
    }

    /// 客户端目录联接：显式开启环境下，真实目录存在 ⇒ 联接并**可读穿透**；不存在 ⇒ 跳过；
    /// 预览侧已有同名目录 ⇒ 绝不覆盖。
    #[test]
    fn links_existing_client_dirs_readonly_through() {
        let _env = PreviewTestEnvGuard::new(Some("1"), None);
        assert!(preview_client_dirs_link_enabled(), "解析函数必须为真");
        assert!(parse_preview_link_client_dirs_value(
            std::env::var(PREVIEW_LINK_CLIENT_DIRS_ENV).ok().as_deref()
        )
        .is_enabled());

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let real_home = std::env::temp_dir().join(format!("fusion-link-real-{stamp}"));
        let root = std::env::temp_dir().join(format!("fusion-link-root-{stamp}"));
        std::fs::create_dir_all(real_home.join(".qoder-cn").join("projects")).unwrap();
        std::fs::write(
            real_home.join(".qoder-cn").join("projects").join("s.jsonl"),
            "{\"credits\":1}",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".workbuddy")).unwrap();

        link_external_client_dirs(&root, &real_home);

        // ① 存在的真实客户端目录 ⇒ 联接可读穿透。
        let through = root.join(".qoder-cn").join("projects").join("s.jsonl");
        assert_eq!(
            std::fs::read_to_string(&through).unwrap(),
            "{\"credits\":1}",
            "联接必须可读穿透到真实客户端数据"
        );
        // ② 真实目录不存在 ⇒ 不造联接。
        assert!(!root.join(".qoder").symlink_metadata().is_ok());
        // ③ 预览侧已有同名目录 ⇒ 不覆盖。
        assert!(root.join(".workbuddy").symlink_metadata().is_ok());
        assert!(!root.join(".workbuddy").join("should-not-appear").exists());

        // 清理：只摘联接/临时目录，不碰真实目录（本测试的 real_home 本身就是临时目录）。
        let _ = std::fs::remove_dir(root.join(".qoder-cn"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(real_home);
    }

    /// 默认环境（BUDDY_SWITCH_PREVIEW_LINK_CLIENT_DIRS 未设）⇒ 解析为关；
    /// 在 env guard 将 BUDDY_SWITCH_HOME 指向临时目录后调用 initialize()，
    /// 断言临时 root 下不存在 CLIENT_LINK_DIRS 的任何成员。
    #[test]
    fn default_environment_disables_client_dirs_linking() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let isolated_root =
            std::env::temp_dir().join(format!("fusion-preview-default-off-{stamp}"));
        std::fs::create_dir_all(&isolated_root).unwrap();

        let _env = PreviewTestEnvGuard::new(None, Some(&isolated_root));
        assert!(
            !preview_client_dirs_link_enabled(),
            "默认环境未设置变量时必须解析为关"
        );

        let initialized_root = initialize().expect("在隔离目录初始化整合预览版应该成功");
        assert_eq!(
            initialized_root,
            isolated_root.canonicalize().unwrap(),
            "初始化返回的必须是隔离的临时目录"
        );

        for dir in CLIENT_LINK_DIRS {
            let link = isolated_root.join(dir);
            assert!(
                link.symlink_metadata().is_err(),
                "默认关闭联接时，临时目录不得创建任何客户端目录联接（发现存在：{dir}）"
            );
        }

        let _ = std::fs::remove_dir_all(isolated_root);
    }

    /// 开关各值解析矩阵（缺省/0/false/off/no/1/true/on/yes/大小写/含空白/垃圾值）。
    #[test]
    fn preview_link_client_dirs_parse_matrix() {
        // 缺省与空白 ⇒ Off，不启用
        for raw in [None, Some(""), Some("   "), Some("\t\n")] {
            let outcome = parse_preview_link_client_dirs_value(raw);
            assert_eq!(outcome, SwitchParseOutcome::Off, "{raw:?} 应解析为 Off");
            assert!(!outcome.is_enabled());
        }

        // 明确关闭值（大小写/带空白）⇒ Off
        for raw in [
            "0",
            " 0 ",
            "false",
            "False",
            "FALSE",
            " false \t",
            "off",
            "Off",
            "OFF",
            " off ",
            "no",
            "No",
            "NO",
            " no ",
        ] {
            let outcome = parse_preview_link_client_dirs_value(Some(raw));
            assert_eq!(outcome, SwitchParseOutcome::Off, "{raw:?} 应解析为 Off");
            assert!(!outcome.is_enabled());
        }

        // 明确开启值（大小写/带空白）⇒ On
        for raw in [
            "1", " 1 ", "true", "True", "TRUE", " true \n", "on", "On", "ON", " on ", "yes", "Yes",
            "YES", " yes ",
        ] {
            let outcome = parse_preview_link_client_dirs_value(Some(raw));
            assert_eq!(outcome, SwitchParseOutcome::On, "{raw:?} 应解析为 On");
            assert!(outcome.is_enabled());
        }

        // 无法识别的值 ⇒ Unrecognized，视为关
        for raw in [
            "2", "garbage", "enabled", "disable", "yess", "true1", "invalid", "!@#",
        ] {
            let outcome = parse_preview_link_client_dirs_value(Some(raw));
            assert_eq!(
                outcome,
                SwitchParseOutcome::Unrecognized,
                "{raw:?} 应解析为 Unrecognized"
            );
            assert!(!outcome.is_enabled());
        }
    }
}
