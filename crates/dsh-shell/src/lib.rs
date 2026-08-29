//! dsh-shell — a shell-command tool.
//!
//! One `shell` tool that runs a command line (through `cmd /C` on Windows,
//! `sh -c` elsewhere) and returns combined stdout/stderr, with a foreground
//! timeout（对齐 `packages/shell/bash-local`：默认 120s，每调用 `timeout_ms`
//! 覆盖且上限 `max_timeout_ms`，超时杀进程并报错）。进程等待在 tokio 运行时
//! 内完成；`kill_on_drop` 保证超时/取消后不留孤儿进程。
//!
//! PTY 会话：参考实现同样推迟（bash-local 的 XXX 注记——持久 cwd 与 PTY
//! 会话留待工作流需要时再做），这里保持一致。

use std::time::Duration;

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

/// 默认前台超时（web bash-local 默认）。
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// 每调用覆盖的上限。
pub const MAX_TIMEOUT_MS: u64 = 600_000;

pub struct ShellTool {
    default_timeout_ms: u64,
    max_timeout_ms: u64,
    /// 会话工作目录（web session.header.cwd；None = 进程 cwd）
    workdir: dsh_tools::Workdir,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
            max_timeout_ms: MAX_TIMEOUT_MS,
            workdir: dsh_tools::Workdir::new(),
        }
    }
}

impl ShellTool {
    pub fn new(default_timeout_ms: u64, max_timeout_ms: u64) -> Self {
        Self {
            default_timeout_ms: default_timeout_ms.max(1),
            max_timeout_ms: max_timeout_ms.max(default_timeout_ms.max(1)),
            workdir: dsh_tools::Workdir::new(),
        }
    }

    /// 注入会话工作目录（命令在此目录下执行）。
    pub fn with_workdir(mut self, workdir: dsh_tools::Workdir) -> Self {
        self.workdir = workdir;
        self
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".into(),
            description: "Run a shell command and return its stdout and stderr.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." },
                    "timeout_ms": { "type": "integer", "description": format!("Foreground timeout in milliseconds (default {}, max {}).", self.default_timeout_ms, self.max_timeout_ms) }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let command = input.arguments.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if command.trim().is_empty() {
            return ToolExecutionResult::error("command must be a non-empty string");
        }
        let timeout_ms = input
            .arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_ms)
            .clamp(1, self.max_timeout_ms);
        run(&command, timeout_ms, &self.workdir).await
    }
}

async fn run(command: &str, timeout_ms: u64, workdir: &dsh_tools::Workdir) -> ToolExecutionResult {
    use tokio::io::AsyncReadExt;

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(workdir.get().unwrap_or_else(|| std::env::current_dir().unwrap_or_default()));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolExecutionResult::error(format!("spawn failed: {e}")),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // 三路并发：读 stdout / 读 stderr / 等退出；整体受前台超时约束
    let read_out = async move {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    };
    let read_err = async move {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    };
    let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let (out, err, status) = tokio::join!(read_out, read_err, child.wait());
        (out, err, status)
    })
    .await;
    match waited {
        Ok((out, err, Ok(status))) => {
            let mut text = String::from_utf8_lossy(&out).into_owned();
            if !err.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&String::from_utf8_lossy(&err));
            }
            if status.success() {
                ToolExecutionResult::text(text)
            } else {
                ToolExecutionResult::error(text)
            }
        }
        Ok((_, _, Err(e))) => ToolExecutionResult::error(format!("spawn failed: {e}")),
        Err(_) => {
            // 超时：杀进程（kill_on_drop 兜底孤儿），报错并说明预算
            let _ = child.kill().await;
            ToolExecutionResult::error(format!("command timed out after {timeout_ms}ms"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::types::CallId;

    fn input(raw: &str) -> ToolExecutionInput {
        ToolExecutionInput::with_raw_arguments(CallId("t".into()), "shell".into(), raw.into())
    }

    #[tokio::test]
    async fn runs_and_returns_output() {
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "echo hello"}"#)).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("hello"), "{text}");
    }

    #[tokio::test]
    async fn times_out_and_kills() {
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "sleep 5", "timeout_ms": 100}"#)).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(is_error);
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("timed out after 100ms"), "{text}");
    }

    #[tokio::test]
    async fn runs_in_injected_workdir() {
        let dir = std::env::temp_dir().join(format!("dsh-shell-wd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = ShellTool::default().with_workdir(dsh_tools::Workdir::with_value(dir.clone()));
        let result = tool.execute(&input(r#"{"command": "basename \"$PWD\""}"#)).await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains(&format!("dsh-shell-wd-{}", std::process::id())), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn clamps_timeout_override_to_max() {
        let tool = ShellTool::new(1000, 200);
        // 覆盖 5000 被 clamp 到 200：sleep 5 应在 200ms 预算内超时
        let result = tool.execute(&input(r#"{"command": "sleep 5", "timeout_ms": 5000}"#)).await;
        assert!(result.is_error);
    }
}
