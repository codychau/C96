//! CLI → GUI 启动桥（阶段 E 配套）。
//!
//! 双击 `C96.exe`（无参数）或在终端执行 `C96 gui` 时，拉起独立 crate `gui/`
//! 的 Tauri 进程 `c96-gui.exe`。只用 std（`std::process::Command`），零新依赖，
//! 核心仍可完全离线编译。

use std::path::{Path, PathBuf};

// Windows shell API：用来「不继承父进程句柄」地启动 GUI。
#[cfg(windows)]
#[allow(non_snake_case)]
#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: *mut core::ffi::c_void,
        lpoperation: *const u16,
        lpfile: *const u16,
        lpparameters: *const u16,
        lpdirectory: *const u16,
        nshowcmd: i32,
    ) -> *mut core::ffi::c_void;
}

/// GUI 可执行文件名。
pub const GUI_EXE: &str = "c96-gui.exe";

/// 按优先级给出候选 GUI 路径（纯函数，便于单测）。
/// - `exe_dir`：CLI 自身所在目录
/// - `cwd`：当前工作目录
pub fn candidates(exe_dir: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    // 1) 与 CLI 同目录（把 GUI 拷过来 / 打包分发时）
    v.push(exe_dir.join(GUI_EXE));
    // 2) 仓内布局：<root>/target/<profile>/C96.exe → <root>/gui/target/<profile>/c96-gui.exe
    //    release 优先（GUI 子系统，双击无控制台黑窗）
    if let Some(root) = exe_dir.parent().and_then(|p| p.parent()) {
        v.push(root.join("gui").join("target").join("release").join(GUI_EXE));
        v.push(root.join("gui").join("target").join("debug").join(GUI_EXE));
    }
    // 3) 相对当前工作目录（在 <root> 下运行）
    v.push(cwd.join("gui").join("target").join("release").join(GUI_EXE));
    v.push(cwd.join("gui").join("target").join("debug").join(GUI_EXE));
    return v;
}

/// 返回第一个真实存在的候选路径。
pub fn find_gui(exe_dir: &Path, cwd: &Path) -> Option<PathBuf> {
    for p in candidates(exe_dir, cwd) {
        if p.is_file() {
            return Some(p);
        }
    }
    return None;
}

fn gui_dir(gui: &Path, cwd: &Path) -> PathBuf {
    return match gui.parent() {
        Some(d) => d.to_path_buf(),
        None => cwd.to_path_buf(),
    };
}

/// Windows：走 shell（ShellExecuteW）启动。
///
/// 用 `std::process::Command` 会让 GUI 子进程继承父进程的 stdout 管道（Rust 在 Windows
/// 上创建进程时 inherit_handles=TRUE），于是 `C96 gui | more` 这类带管道的调用会一直挂住
/// 直到 GUI 退出。ShellExecuteW 由 shell 创建进程，不继承调用方的句柄。
#[cfg(windows)]
fn spawn(gui: &Path, cwd: &Path, silent: bool) -> Result<PathBuf, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    fn wide(s: &OsStr) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_wide().collect();
        v.push(0);
        return v;
    }

    let file = wide(gui.as_os_str());
    let op = wide(OsStr::new("open"));
    let dir = wide(gui_dir(gui, cwd).as_os_str());
    // 静默：给 GUI 传 --silent，让它以隐藏窗口方式启动（已运行则不唤窗）。
    let params = wide(OsStr::new("--silent"));
    let params_ptr: *const u16 = if silent { params.as_ptr() } else { core::ptr::null() };
    let rc = unsafe {
        ShellExecuteW(
            core::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params_ptr,
            dir.as_ptr(),
            1,
        )
    };
    let code = rc as isize;
    if code <= 32 {
        return Err(format!("ShellExecuteW failed (code {}) for {}", code, gui.display()));
    }
    return Ok(gui.to_path_buf());
}

/// 非 Windows：普通分离进程。
#[cfg(not(windows))]
fn spawn(gui: &Path, cwd: &Path, silent: bool) -> Result<PathBuf, String> {
    let mut cmd = std::process::Command::new(gui);
    cmd.current_dir(gui_dir(gui, cwd))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if silent {
        cmd.arg("--silent");
    }
    cmd.spawn()
        .map_err(|e| format!("spawn {} failed: {}", gui.display(), e))?;
    return Ok(gui.to_path_buf());
}

/// 拉起 GUI 进程（分离启动，立即返回）。
///
/// `silent=true`（`C96 gui --silent`）：透传 `--silent` 给 GUI —— 未启动则以隐藏
/// 窗口方式启动；已启动则不唤起其窗口（由 GUI 的单实例逻辑处理）。
///
/// 可用环境变量 `C96_GUI` 显式指定 GUI 可执行文件路径。
pub fn launch(silent: bool) -> Result<PathBuf, String> {
    let exe_dir: PathBuf = match std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        Some(d) => d,
        None => return Err("cannot resolve CLI executable directory".to_string()),
    };
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    if let Ok(env_path) = std::env::var("C96_GUI") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return spawn(&p, &cwd, silent);
        }
    }
    match find_gui(&exe_dir, &cwd) {
        Some(p) => return spawn(&p, &cwd, silent),
        None => {
            let tried: Vec<String> = candidates(&exe_dir, &cwd).iter().map(|p| p.display().to_string()).collect();
            return Err(format!(
                "未找到 {}。先执行：cd gui; cargo build --release。候选：{}",
                GUI_EXE,
                tried.join(" | ")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dir_candidate_is_first() {
        let exe_dir = PathBuf::from("C:/repo/target/release");
        let cwd = PathBuf::from("C:/repo");
        let c = candidates(&exe_dir, &cwd);
        assert_eq!(c[0], exe_dir.join(GUI_EXE));
    }

    #[test]
    fn repo_layout_release_before_debug() {
        let exe_dir = PathBuf::from("C:/repo/target/release");
        let cwd = PathBuf::from("C:/repo");
        let c = candidates(&exe_dir, &cwd);
        let root = exe_dir.parent().and_then(|p| p.parent()).unwrap();
        assert_eq!(c[1], root.join("gui").join("target").join("release").join(GUI_EXE));
        assert_eq!(c[2], root.join("gui").join("target").join("debug").join(GUI_EXE));
        assert!(c[3].starts_with(&cwd));
    }

    #[test]
    fn find_gui_returns_none_for_missing_paths() {
        let exe_dir = PathBuf::from("C:/__c96_no_such__/target/release");
        let cwd = PathBuf::from("C:/__c96_no_such__");
        assert!(find_gui(&exe_dir, &cwd).is_none());
    }
}
