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
        let shell_line = match shell_kind() {
            // bash：模型惯用语法（管道/;/引号/$()）原样生效
            "bash" => "The command runs through bash (Git Bash on Windows), so bash syntax — \
                       pipes, `;`, `$()`, quotes — works naturally. The working directory is \
                       already the session workspace root: do not `cd` first.",
            // cmd：bash 语法会碎裂（; 不分段、引号剥离、% 变量展开）
            _ => "The command runs through `cmd /C` (NOT bash): use `&&` instead of `;`, avoid \
                   `$()` and `%` characters, and expect quotes around arguments with spaces to \
                   be stripped. The working directory is already the session workspace root: do \
                   not `cd` first, and use Windows-style paths.",
        };
        ToolDefinition {
            name: "shell".into(),
            description: format!("Run a shell command and return its combined stdout and stderr. {shell_line}"),
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

/// Windows 上优先 Git Bash：模型惯用 bash 语法（`;`、`$()`、引号、
/// `--format="%ci"`），经 `cmd /C` 全部碎裂（cmd 剥引号、`%` 变量展开、
/// `;` 不分段——真实会话 8 个错误 6 条重试链皆源于此）。对齐参考实现
/// bash-local 的 bash 语义；找不到 bash 再退 `cmd /C`。
#[cfg(windows)]
fn shell_program() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static BASH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    BASH.get_or_init(|| {
        // PATH 上的 bash（Git Bash 终端环境）
        if let Ok(path_var) = std::env::var("PATH") {
            let found = std::env::split_paths(&path_var)
                .map(|dir| dir.join("bash.exe"))
                .find(|p| p.is_file());
            if found.is_some() {
                return Some(std::path::PathBuf::from("bash"));
            }
        }
        // GUI 进程的 PATH 常只有 Git\cmd；按安装位次找包装器 bash.exe
        // （Git\bin\bash.exe 会先把 usr/bin 前置进 PATH，grep/sed 可用）
        let mut candidates = Vec::new();
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                candidates.push(std::path::PathBuf::from(base).join("Git").join("bin").join("bash.exe"));
            }
        }
        candidates.into_iter().find(|p| p.is_file())
    })
    .clone()
}

#[cfg(not(windows))]
fn shell_program() -> Option<std::path::PathBuf> {
    None
}

/// 本进程实际的 shell 风味（"bash" | "cmd"）：供提示词 section 与测试对齐。
pub fn shell_kind() -> &'static str {
    #[cfg(windows)]
    {
        if shell_program().is_some() {
            "bash"
        } else {
            "cmd"
        }
    }
    #[cfg(not(windows))]
    {
        "bash"
    }
}

async fn run(command: &str, timeout_ms: u64, workdir: &dsh_tools::Workdir) -> ToolExecutionResult {
    use tokio::io::AsyncReadExt;

    #[cfg(target_os = "windows")]
    let mut cmd = match shell_program() {
        Some(bash) => {
            let mut c = tokio::process::Command::new(bash);
            c.args(["-c", command]);
            c
        }
        None => {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", command]);
            c
        }
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
    #[cfg(windows)]
    {
        // GUI 宿主无控制台可继承：不设此标志，每个命令都会弹出黑色
        // 控制台窗口并抢前台（真实使用中每次工具执行闪一个 bash 窗）
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
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
            let mut text = decode_output(&out);
            if !err.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&decode_output(&err));
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

/// 输出解码：UTF-8 优先（rust 工具链/现代程序的原生编码）；字节不是合法
/// UTF-8 时按系统代码页兜底——Windows 上 `cmd /C` 的输出（含其错误信息）
/// 用 OEM/控制台代码页编码（中文系统为 GBK/cp936），直接 lossy 解会成乱码，
/// 模型读不懂就无法自诊断。
fn decode_output(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => text,
        Err(_) => decode_system_codepage(bytes),
    }
}

/// 当前 Windows 代码页：有控制台用控制台输出 CP，否则回退 OEM CP。
#[cfg(windows)]
fn system_codepage() -> u16 {
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn GetOEMCP() -> u32;
    }
    let cp = unsafe { GetConsoleOutputCP() };
    let cp = if cp == 0 { unsafe { GetOEMCP() } } else { cp };
    u16::try_from(cp).unwrap_or(0)
}

#[cfg(windows)]
fn decode_system_codepage(bytes: &[u8]) -> String {
    let enc = codepage::to_encoding(system_codepage()).unwrap_or(encoding_rs::UTF_8);
    enc.decode(bytes).0.into_owned()
}

#[cfg(not(windows))]
fn decode_system_codepage(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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
        // 命令按风味选：bash → pwd；cmd /C → 裸 `cd` 打印当前目录
        let raw = if shell_kind() == "bash" {
            r#"{"command": "pwd"}"#
        } else {
            r#"{"command": "cd"}"#
        };
        let result = tool.execute(&input(raw)).await;
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

    #[test]
    fn utf8_output_passes_through() {
        assert_eq!(decode_output("héllo ✔".as_bytes()), "héllo ✔");
    }

    #[tokio::test]
    async fn bash_semantics_work_when_bash_is_flavor() {
        // 真实会话的 8 个错误全是 bash 语法经 cmd /C 碎裂（; 不分段、引号
        // 剥离、% 变量展开）；bash 风味下必须原样生效。
        if shell_kind() != "bash" {
            return;
        }
        let tool = ShellTool::default();
        // `;` 分隔 + 引号内空格 + % 字符，三件套同时验证
        let result = tool
            .execute(&input(
                r#"{"command": "echo one; printf '%s %s\\n' 'a %b' 'c'; echo three"}"#,
            ))
            .await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("one") && text.contains("a %b c") && text.contains("three"), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn shell_kind_matches_available_program() {
        // 味道判定与实际 spawn 程序一致：bash 存在则为 bash，否则 cmd
        let has_bash = shell_program().is_some();
        assert_eq!(shell_kind() == "bash", has_bash);
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_output_decodes_via_system_codepage() {
        // 「系统找不到指定的路径」的 GBK（cp936）字节；仅在 OEM CP 为 936
        // 的系统上断言内容，其他代码页只要求不 panic（回退 UTF-8/其他编码）。
        let gbk = [0xcf, 0xb5, 0xcd, 0xb3, 0xd5, 0xd2, 0xb2, 0xbb, 0xb5, 0xbd, 0xd6, 0xb8, 0xb6, 0xa8, 0xb5, 0xc4, 0xc2, 0xb7, 0xbe, 0xb6];
        let decoded = decode_output(&gbk);
        if system_codepage() == 936 {
            assert_eq!(decoded, "系统找不到指定的路径");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_non_utf8_stderr_is_readable() {
        // 端到端：cmd 的中文输出经管道回来是代码页字节，解码后应可读、无 U+FFFD
        if system_codepage() != 936 {
            return;
        }
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "echo 系统找不到指定的路径"}"#)).await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("系统找不到指定的路径"), "{text}");
        assert!(!text.contains('\u{FFFD}'), "{text}");
    }
}
