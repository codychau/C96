//! 默认相对路径的可移植解析（搬目录安装用）。
//!
//! 默认值本来是相对**当前工作目录**的（\`config/feeds.toml\`、\`cache/http_cache.toml\`）：
//! 在 exe 所在目录里双击没问题，但 \`D:\apps\c96\C96.exe feeds list\` 这类从别处调用就找不到配置。
//! 规则（保持向后兼容，报错信息不变）：
//!   1. CWD 下存在 -> 用原相对路径（旧行为）
//!   2. 否则 exe 目录下存在 -> 用 exe 目录（便携安装：配置跟着 exe 走）
//!   3. 都不存在 -> 仍返回原相对路径

use std::path::Path;

/// 纯逻辑版本（便于单测）：cwd 是否已有该文件 + 可选的 exe 目录。
pub fn pick(rel: &str, cwd_has: bool, exe_dir: Option<&Path>) -> String {
    if cwd_has { return rel.to_string(); }
    if let Some(dir) = exe_dir {
        let alt = dir.join(rel);
        if alt.is_file() { return alt.to_string_lossy().to_string(); }
    }
    return rel.to_string();
}

/// 解析默认相对路径：CWD 优先，其次 exe 目录。
pub fn resolve_default(rel: &str) -> String {
    let cwd_has = Path::new(rel).is_file();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    return pick(rel, cwd_has, exe_dir.as_deref());
}

#[cfg(test)]
mod tests {
    use super::pick;
    use std::path::PathBuf;

    #[test]
    fn cwd_wins_then_exe_dir_then_original() {
        let dir = PathBuf::from("_scratch").join("f_paths_exe");
        let _ = std::fs::create_dir_all(dir.join("config"));
        let _ = std::fs::write(dir.join("config").join("feeds.toml"), "# probe\n");
        assert_eq!(pick("config/feeds.toml", true, Some(&dir)), "config/feeds.toml");
        // 注意：pick 内部就是 dir.join(rel)，斜杠不会被拆开
        assert_eq!(pick("config/feeds.toml", false, Some(&dir)),
            dir.join("config/feeds.toml").to_string_lossy().to_string());
        assert_eq!(pick("config/feeds.toml", false, None), "config/feeds.toml");
        assert_eq!(pick("config/nope.toml", false, Some(&dir)), "config/nope.toml");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
