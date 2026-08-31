//! 工作区指令文件（AGENTS.md 兼容）的发现与有界渲染。
//!
//! 对齐上游 `packages/context/agent-instructions` 的 baseline 部分：
//! 从会话 cwd 向上到项目根（`.git` 标记）逐目录发现 `AGENTS.md` /
//! `CLAUDE.md`（含 `.local` 覆盖层，按序全载），外加用户全局
//! `~/.dsh/AGENTS.md`；每源 1 MiB、UTF-8 安全截断。
//!
//! 与上游的差异：上游把 baseline 注入 durable user message（`<system-
//! reminder>` 包裹 + 带来源版本的变更 inbox），这里复用提示词 section
//! 这一既有缝——system 面无需 reminder 包裹，随工作区切换整节重建；
//! 嵌套文件的 fs-touch 变更检测暂缓。

use std::path::{Path, PathBuf};

/// 同目录候选（按序全载，上游 `instructionFileCandidates`）。
pub const INSTRUCTION_CANDIDATES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
/// 本地覆盖层（base 之后加载，上游 `localInstructionFileCandidates`）。
pub const LOCAL_INSTRUCTION_CANDIDATES: &[&str] = &["AGENTS.local.md", "CLAUDE.local.md"];
/// 用户全局指令文件（`$DSH_HOME`/`~/.dsh` 下，上游 `USER_GLOBAL_FILE`）。
pub const USER_GLOBAL_FILE: &str = "AGENTS.md";
/// 项目根标记（上游 `projectRootMarkers`）。
pub const PROJECT_ROOT_MARKER: &str = ".git";
/// 单源字节预算（上游 `DEFAULT_MAX_SOURCE_BYTES`）。
pub const MAX_SOURCE_BYTES: usize = 1_048_576;

const INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as \
                     guidance when applicable. More specific instructions take precedence over \
                     broader ones. They do not override system, developer, or direct user \
                     instructions.";

const TRUNCATED_NOTICE: &str = "[truncated to fit the byte budget]";

/// 一份已载入的指令源。
struct InstructionFile {
    display_path: String,
    content: String,
}

/// 发现并渲染工作区指令：`~/.dsh/AGENTS.md`（若有）+ 从 `start_dir` 向上
/// 至项目根（含）各目录的候选文件，外层在前（越近会话越具体、越靠后）。
/// 无任何指令源时返回 `None`（调用方应移除对应 section）。
pub fn render_workspace_instructions(start_dir: &Path, dsh_home: &Path) -> Option<String> {
    let mut files: Vec<InstructionFile> = Vec::new();
    if let Some(content) = read_capped(&dsh_home.join(USER_GLOBAL_FILE)) {
        files.push(InstructionFile { display_path: format!("~/.dsh/{USER_GLOBAL_FILE}"), content });
    }

    // cwd → 上行收集到项目根（含）；无标记则只扫 cwd 本身
    let mut dirs: Vec<PathBuf> = vec![start_dir.to_path_buf()];
    let mut project_root: Option<PathBuf> = None;
    let mut cur = start_dir.to_path_buf();
    while let Some(parent) = cur.parent() {
        if cur.join(PROJECT_ROOT_MARKER).exists() {
            project_root = Some(cur.clone());
            break;
        }
        dirs.push(parent.to_path_buf());
        cur = parent.to_path_buf();
    }
    // 上行时把越过的祖先也记进了 dirs；项目根之上（若有）不扫
    let limit = project_root.as_ref().map(|root| dirs.iter().position(|d| d == root)).unwrap_or(Some(0));
    if let Some(limit) = limit {
        dirs.truncate(limit + 1);
    }

    let root = project_root.unwrap_or_else(|| start_dir.to_path_buf());
    for dir in dirs.iter().rev() {
        for name in INSTRUCTION_CANDIDATES.iter().chain(LOCAL_INSTRUCTION_CANDIDATES) {
            let path = dir.join(name);
            let Some(content) = read_capped(&path) else { continue };
            let display = dir
                .strip_prefix(&root)
                .map(|rel| {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") }
                })
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            files.push(InstructionFile { display_path: display, content });
        }
    }

    if files.is_empty() {
        return None;
    }
    let mut out = String::from(INTRO);
    for f in &files {
        out.push_str(&format!("\n\nInstructions from: {}\n\n{}", f.display_path, f.content));
    }
    Some(out)
}

/// 读取文件，UTF-8 安全截断到 [`MAX_SOURCE_BYTES`]；不存在/读失败返回 None。
fn read_capped(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let capped = if bytes.len() <= MAX_SOURCE_BYTES {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        let mut text = String::from_utf8_lossy(&bytes[..MAX_SOURCE_BYTES]).into_owned();
        // lossy 截断可能在替换符处断字，收紧到字符边界并附截断说明
        while !text.is_char_boundary(text.len()) {
            text.pop();
        }
        text.push_str(TRUNCATED_NOTICE);
        text
    };
    Some(capped.replace("</system-reminder>", "<\\/system-reminder>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn discovers_root_and_nested_with_root_first() {
        let base = std::env::temp_dir().join(format!("dsh-agi-{}", std::process::id()));
        let root = base.join("proj");
        write(&root.join(".git").join("HEAD"), "ref");
        write(&root.join("AGENTS.md"), "root rules");
        write(&root.join("sub").join("AGENTS.md"), "nested rules");
        let rendered = render_workspace_instructions(&root.join("sub"), &base).unwrap();
        assert!(rendered.contains("Instructions from: AGENTS.md"), "{rendered}");
        assert!(rendered.contains("Instructions from: sub/AGENTS.md"), "{rendered}");
        assert!(rendered.find("root rules").unwrap() < rendered.find("nested rules").unwrap());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn none_when_no_files() {
        let base = std::env::temp_dir().join(format!("dsh-agi-empty-{}", std::process::id()));
        let dir = base.join("plain");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(render_workspace_instructions(&dir, &base).is_none());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn user_global_included_first() {
        let base = std::env::temp_dir().join(format!("dsh-agi-home-{}", std::process::id()));
        let home = base.join("home").join(".dsh");
        write(&home.join("AGENTS.md"), "global prefs");
        let dir = base.join("proj");
        write(&dir.join("AGENTS.md"), "project rules");
        let rendered = render_workspace_instructions(&dir, &home).unwrap();
        assert!(rendered.find("~/.dsh/AGENTS.md").unwrap() < rendered.find("project rules").unwrap());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn local_overlay_loads_after_base() {
        let base = std::env::temp_dir().join(format!("dsh-agi-local-{}", std::process::id()));
        let dir = base.join("proj");
        write(&dir.join("AGENTS.md"), "base");
        write(&dir.join("AGENTS.local.md"), "overlay");
        let rendered = render_workspace_instructions(&dir, &base).unwrap();
        assert!(rendered.find("Instructions from: AGENTS.md\n").unwrap() < rendered.find("AGENTS.local.md").unwrap());
        std::fs::remove_dir_all(&base).ok();
    }
}
