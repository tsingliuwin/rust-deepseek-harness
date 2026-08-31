//! dsh-fs — a local-filesystem tool.
//!
//! One `fs` tool exposing `read` / `write` / `list` / `exists` operations over
//! `std::fs`, behind an injected [`FsPolicy`]（web `fs-sandbox` containment +
//! `fs-observation-policy` 的合并缝）：
//!
//! - [`AllowAllPolicy`]：直通（默认，无沙箱）。
//! - [`WorkspaceContainment`]：写限定在给定工作区根之下（含 `~` 展开与
//!   规范化——目标不存在时回退到最近存在祖先再判包含，web containment
//!   的同一保守语义）；读/列/存在检查不受限。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

/// 文件系统策略 provider：读可见性 + 写包含。
pub trait FsPolicy: Send + Sync {
    /// `read` / `list` / `exists` 是否允许。
    fn allow_read(&self, path: &Path) -> bool {
        let _ = path;
        true
    }
    /// `write` 是否允许。
    fn allow_write(&self, path: &Path) -> bool {
        let _ = path;
        true
    }
    /// 拒绝时的错误前缀（策略语义说明）。
    fn deny_reason(&self) -> &'static str {
        "path denied by fs policy"
    }
}

/// 直通策略（默认）：全部允许，无沙箱。
pub struct AllowAllPolicy;

impl FsPolicy for AllowAllPolicy {}

/// 工作区包含策略：写限定在根集合之下。
pub struct WorkspaceContainment {
    roots: RwLock<Vec<PathBuf>>,
}

impl WorkspaceContainment {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots: RwLock::new(roots) }
    }

    /// 宿主切换工作区时更新根集合。
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.roots.write().unwrap() = roots;
    }

    /// 目标是否落在任一根之下（词法规范化；`~` 展开）。
    fn under_any(&self, path: &Path) -> bool {
        let roots = self.roots.read().unwrap();
        if roots.is_empty() {
            return false;
        }
        let expanded = expand_home(path);
        let canonical = canonicalize_best_effort(&expanded);
        roots.iter().any(|root| {
            let root_expanded = expand_home(root);
            let root_canonical = canonicalize_best_effort(&root_expanded);
            canonical == root_canonical || canonical.starts_with(&root_canonical)
        })
    }
}

impl FsPolicy for WorkspaceContainment {
    fn allow_write(&self, path: &Path) -> bool {
        self.under_any(path)
    }

    fn deny_reason(&self) -> &'static str {
        "write denied: path is outside the workspace sandbox"
    }
}

/// 展开 `~` / `~/` 前缀。
fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{home}/{rest}"));
        }
    } else if text == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    path.to_path_buf()
}

/// 尽力规范化：目标不存在（写新文件）时回退到最近存在的祖先，再接回
/// 剩余段（web containment 的 ancestor-walk 保守等价）。
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut ancestor = path.to_path_buf();
    let mut suffix: Vec<PathBuf> = Vec::new();
    while let Some(parent) = ancestor.parent() {
        suffix.push(ancestor.file_name().map(PathBuf::from).unwrap_or_default());
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for seg in suffix.into_iter().rev() {
                out.push(seg);
            }
            return out;
        }
        ancestor = parent.to_path_buf();
    }
    path.to_path_buf()
}

/// `fs` 工具：所有操作经注入的策略检查后落到 `std::fs`。
/// 相对路径按会话工作目录展开（web session.header.cwd 语义）。
pub struct FsTool {
    policy: Arc<dyn FsPolicy>,
    workdir: dsh_tools::Workdir,
}

impl FsTool {
    pub fn new(policy: Arc<dyn FsPolicy>) -> Self {
        Self { policy, workdir: dsh_tools::Workdir::new() }
    }

    /// 注入会话工作目录（相对路径的解析基准）。
    pub fn with_workdir(mut self, workdir: dsh_tools::Workdir) -> Self {
        self.workdir = workdir;
        self
    }
}

impl Default for FsTool {
    fn default() -> Self {
        Self::new(Arc::new(AllowAllPolicy))
    }
}

#[async_trait]
impl Tool for FsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs".into(),
            description: "Read, write, list, or check files on the local filesystem.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["read", "write", "list", "exists"] },
                    "path": { "type": "string", "description": "Filesystem path." },
                    "content": { "type": "string", "description": "Content to write (write op only)." }
                },
                "required": ["op", "path"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let op = input.arguments.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let path = input.arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let path = self.workdir.resolve(Path::new(path));
        let path = path.as_path();
        if path.as_os_str().is_empty() {
            return ToolExecutionResult::error("path must be a non-empty string");
        }
        match op {
            "read" | "list" | "exists" => {
                if !self.policy.allow_read(path) {
                    return ToolExecutionResult::error(self.policy.deny_reason());
                }
            }
            "write" => {
                if !self.policy.allow_write(path) {
                    return ToolExecutionResult::error(self.policy.deny_reason());
                }
            }
            _ => {}
        }
        match op {
            "read" => {
                // 目录在 Windows 上 read_to_string 报 ERROR_ACCESS_DENIED
                // （"拒绝访问 os error 5"），语义误导模型；显式指路到 list。
                if path.is_dir() {
                    return ToolExecutionResult::error(format!(
                        "read failed: {} is a directory; use op=list",
                        path.display()
                    ));
                }
                match std::fs::read_to_string(path) {
                    Ok(text) => ToolExecutionResult::text(text),
                    Err(e) => ToolExecutionResult::error(format!("read failed: {e}")),
                }
            }
            "write" => {
                let content = input.arguments.get("content").and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::write(path, content) {
                    Ok(()) => ToolExecutionResult::text(format!("wrote {} bytes", content.len())),
                    Err(e) => ToolExecutionResult::error(format!("write failed: {e}")),
                }
            }
            "list" => match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut out = String::new();
                    for entry in entries.flatten() {
                        out.push_str(&entry.file_name().to_string_lossy());
                        out.push('\n');
                    }
                    ToolExecutionResult::text(out)
                }
                Err(e) => ToolExecutionResult::error(format!("list failed: {e}")),
            },
            "exists" => ToolExecutionResult::text(format!("{}", path.exists())),
            other => ToolExecutionResult::error(format!("unknown op \"{other}\"")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_roots(roots: &[&str]) -> WorkspaceContainment {
        WorkspaceContainment::new(roots.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn write_inside_root_allowed_outside_denied() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let policy = policy_with_roots(&[dir.to_str().unwrap()]);
        assert!(policy.allow_write(&dir.join("sub/new.txt")));
        assert!(!policy.allow_write(Path::new("/etc/hosts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nonexistent_target_uses_ancestor_containment() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-test2-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let policy = policy_with_roots(&[dir.join("a").to_str().unwrap()]);
        // 目标 b/c/d.txt 尚不存在：回退到存在的祖先 b 判包含
        assert!(policy.allow_write(&dir.join("a/b/c/d.txt")));
        assert!(!policy.allow_write(&dir.join("outside/x.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_roots_deny_all_writes() {
        let policy = WorkspaceContainment::new(Vec::new());
        assert!(!policy.allow_write(Path::new("/tmp/x")));
    }

    #[tokio::test]
    async fn read_on_directory_points_to_list() {
        // 目录读取曾误报「拒绝访问 os error 5」；应显式提示改用 op=list
        let tool = FsTool::default();
        let dir = std::env::temp_dir().join(format!("dsh-fs-dir-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = tool
            .execute(&ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("t".into()),
                "fs".into(),
                format!(r#"{{"op": "read", "path": "{}"}}"#, dir.to_string_lossy().replace('\\', "\\\\")).into(),
            ))
            .await;
        assert!(result.is_error);
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("is a directory") && text.contains("op=list"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
