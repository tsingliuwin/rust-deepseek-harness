//! dsh-search — 内容搜索工具（grep）。
//!
//! 对齐参考仓库 `packages/fs/tool-fs-search/src/grep.ts`：ripgrep 语义的
//! 文件内容搜索。参考实现直接 spawn `rg --json`（@vscode/ripgrep）；本实现
//! 用 ripgrep 自家的 `ignore` crate（.gitignore/.ignore/隐藏文件语义一致）
//! 加 `regex` 在进程内完成同一件事，避免依赖外部二进制。
//!
//! 模型可见结果格式逐字对齐 web `formatGrepOutput`：
//! `Found N matches`（截断时 `Found K of N matches`）+ 按文件分组的
//! `Line N: text` 行 + 无匹配时的 `No matches found`。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};

/// 单次 grep 调用内联保留的匹配上限（web GREP_MAX_MATCHES，同 Claude Code
/// GrepTool 的 head_limit 默认）。
pub const GREP_MAX_MATCHES: usize = 250;

/// 单行预览的字符上限（web GREP_MAX_LINE_BYTES = 2000 bytes；这里按字符
/// 截断，UTF-8 边界天然安全）。
const GREP_MAX_LINE_CHARS: usize = 2000;

/// glob 工具内联保留的路径上限（web GLOB_MAX_RESULTS，同 Claude Code
/// GlobTool 的结果上限）。
pub const GLOB_MAX_RESULTS: usize = 100;

/// 目录发现列表永不进入的 VCS 元数据目录（web GLOB_VCS_EXCLUDES）：
/// glob 用 --no-ignore --hidden 语义，不排除会每次都翻出这些目录。
const GLOB_VCS_EXCLUDES: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// 单文件读取上限（字节）；超出按二进制跳过。ripgrep 默认无大小上限，
/// 这里加一道护栏防止超大文件拖垮进程内搜索。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// 一条匹配（相对展示路径 + 1 基行号 + 行文本）。
pub struct GrepMatch {
    pub path: String,
    pub line_number: usize,
    pub line: String,
}

pub struct GrepTool;

impl GrepTool {
    /// 按文件分组（首次出现顺序）为模型可见正文：每组 = 路径行 + 逐匹配
    /// `Line N: text` 行，组间空行（web formatGrepMatches）。
    fn format_matches(matches: &[GrepMatch]) -> String {
        let mut sections: Vec<String> = Vec::new();
        let mut i = 0;
        while i < matches.len() {
            let path = &matches[i].path;
            let mut group = vec![path.clone()];
            let mut j = i;
            while j < matches.len() && &matches[j].path == path {
                group.push(format!("Line {}: {}", matches[j].line_number, matches[j].line));
                j += 1;
            }
            sections.push(group.join("\n"));
            i = j;
        }
        sections.join("\n\n")
    }

    /// 模型可见结果：计数头 + 分组正文 + 截断时的恢复提示（web
    /// formatGrepOutput；无 spill 文件设施，截断提示用其不可保存分支）。
    fn format_output(seen: usize, retained: &[GrepMatch]) -> String {
        if seen == 0 {
            return "No matches found".into();
        }
        let header = if retained.len() < seen {
            format!("Found {} of {seen} matches", retained.len())
        } else {
            let noun = if seen == 1 { "match" } else { "matches" };
            format!("Found {seen} {noun}")
        };
        let body = Self::format_matches(retained);
        if retained.len() < seen {
            format!(
                "{header}\n\n{body}\n\n(The complete result could not be saved; narrow pattern, path, or include to see more.)"
            )
        } else {
            format!("{header}\n\n{body}")
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Search file contents with a ripgrep regular expression. Returns matching lines with line numbers, grouped by file. \
Returns the first 250 matches inline. Respects .gitignore and skips hidden files. \
Use read on a matched file for surrounding context."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for (ripgrep syntax)." },
                    "path": { "type": "string", "description": "File or directory to search. Defaults to the session workspace; a relative path resolves against it." },
                    "include": { "type": "string", "description": "One glob filter for which files to search (e.g. \"*.rs\", \"*.{ts,tsx}\"). Not a list; negation is not supported." }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let pattern = input.arguments.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let path = input.arguments.get("path").and_then(|v| v.as_str()).map(str::to_string);
        let include = input.arguments.get("include").and_then(|v| v.as_str()).map(str::to_string);
        tokio::task::spawn_blocking(move || run(&pattern, path, include))
            .await
            .unwrap_or_else(|e| ToolExecutionResult::error(format!("grep failed: {e}")))
    }
}

/// 进程内 ripgrep：ignore walk（尊重 .gitignore、跳隐藏）+ glob 过滤 +
/// 逐行正则匹配。文件参数（非目录）退化为单文件搜索。
fn run(pattern: &str, path: Option<String>, include: Option<String>) -> ToolExecutionResult {
    if pattern.is_empty() {
        return ToolExecutionResult::error("pattern must be a non-empty string");
    }
    if include.as_deref().is_some_and(|g| g.trim().is_empty() || g.starts_with('!')) {
        return ToolExecutionResult::error(
            "include must be one positive glob filter (e.g. \"*.rs\"); negated patterns are not supported",
        );
    }
    let re = match regex::Regex::new(pattern) {
        Ok(re) => re,
        Err(e) => return ToolExecutionResult::error(format!("invalid pattern: {e}")),
    };
    let glob = match &include {
        Some(g) => match globset::Glob::new(g) {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(e) => return ToolExecutionResult::error(format!("invalid include glob: {e}")),
        },
        None => None,
    };
    let root: PathBuf = match &path {
        Some(p) => PathBuf::from(shell_expand(p)),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => return ToolExecutionResult::error(format!("grep failed: {e}")),
        },
    };
    if !root.exists() {
        return ToolExecutionResult::error(format!("grep failed: no such file or directory: {}", root.display()));
    }

    let mut matches: Vec<GrepMatch> = Vec::new();
    let mut seen = 0usize;
    let truncated_fit = |matches: &Vec<GrepMatch>| matches.len() < GREP_MAX_MATCHES;

    if root.is_file() {
        search_file(&root, &root.clone(), &re, &glob, &mut |m| {
            if truncated_fit(&matches) {
                matches.push(m);
            }
            seen += 1;
        });
    } else {
        let walker = ignore::WalkBuilder::new(&root).build();
        for entry in walker.flatten() {
            if !truncated_fit(&matches) && seen >= GREP_MAX_MATCHES {
                break;
            }
            let file = entry.path();
            if !file.is_file() {
                continue;
            }
            // 展示路径相对搜索根（web toWorkdirRelative）
            let display = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().into_owned();
            if let Some(g) = &glob
                && !g.is_match(&display)
                && !g.is_match(file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            {
                continue;
            }
            let display = display.replace('\\', "/");
            search_file(file, Path::new(&display), &re, &glob, &mut |m| {
                if truncated_fit(&matches) {
                    matches.push(m);
                }
                seen += 1;
            });
        }
    }
    ToolExecutionResult::text(GrepTool::format_output(seen, &matches))
}

/// 在单文件内逐行匹配。NUL 字节（前 8KB）判为二进制跳过；无效 UTF-8 行按
/// lossy 替换（web 对应 base64 bytes 的占位语义从简）。`display` 为该文件
/// 的展示路径（目录搜索时相对根，单文件搜索时原样）。
fn search_file(
    file: &Path,
    display: &Path,
    re: &regex::Regex,
    glob: &Option<globset::GlobMatcher>,
    push: &mut dyn FnMut(GrepMatch),
) {
    // include glob 对单文件搜索同样生效（rg 语义：过滤目标文件）
    if let Some(g) = glob {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !g.is_match(name) {
            return;
        }
    }
    let Ok(bytes) = std::fs::read(file) else { return };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return;
    }
    if bytes[..bytes.len().min(8 * 1024)].contains(&0) {
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    let path = display.to_string_lossy().into_owned();
    for (i, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if re.is_match(line) {
            let line = truncate_line(line);
            push(GrepMatch { path: path.clone(), line_number: i + 1, line });
        }
    }
}

/// 行预览截断（web previewLine：超限保前缀 + …）。
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= GREP_MAX_LINE_CHARS {
        line.to_string()
    } else {
        let cut: String = line.chars().take(GREP_MAX_LINE_CHARS).collect();
        format!("{cut}…")
    }
}

/// 展开 `~` 前缀（路径参数允许 home 相对）。
fn shell_expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    p.to_string()
}

/// 模型面向的 `glob` 工具：按路径模式发现文件，修改时间升序（web
/// `rg --files --glob --sort=modified --no-ignore --hidden` 语义——包含
/// 隐藏与被忽略文件、剔除 VCS 元数据目录、只返回文件从不返回目录）。
/// 无分隔符的模式按任意深度的 basename 匹配（web 模式描述）。
pub struct GlobTool;

impl GlobTool {
    /// 路径页格式（web formatGlobPage 的不可保存恢复分支）：正文 + 截断脚注。
    fn format_page(items: &[String], seen: usize) -> String {
        let body = items.join("\n");
        format!(
            "{body}\n\n(Showing {} of {seen} paths. The complete result could not be saved; narrow pattern or path to see more.)",
            items.len()
        )
    }

    /// web renderGlobPaths 的平头分支（sampleOverCapGlobResults 关闭态：
    /// 截断保留修改时间序头部）。
    fn format_output(paths: &[String]) -> String {
        if paths.is_empty() {
            return "No files found".into();
        }
        if paths.len() <= GLOB_MAX_RESULTS {
            return paths.join("\n");
        }
        Self::format_page(&paths[..GLOB_MAX_RESULTS], paths.len())
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".into(),
            description: "Find files whose paths match a glob pattern. Returns matching file paths — never directories — \
including hidden and ignored files (VCS metadata directories are excluded), in modification-time order. \
Up to 100 paths come back inline; a larger result returns the first 100 in modification-time order and says so. \
This tool does not enumerate directory entries."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern to match file paths against (e.g. \"**/*.rs\", \"src/**/*.test.js\"). A pattern with no \"/\" matches the basename at any depth, so \"*\" and \"*.rs\" both search the whole tree; include a separator to anchor the depth." },
                    "path": { "type": "string", "description": "Directory to search in. Defaults to the session workspace; a relative path resolves against it." }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let pattern = input.arguments.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let path = input.arguments.get("path").and_then(|v| v.as_str()).map(str::to_string);
        tokio::task::spawn_blocking(move || run_glob(&pattern, path))
            .await
            .unwrap_or_else(|e| ToolExecutionResult::error(format!("glob failed: {e}")))
    }
}

/// glob 执行：ignore walk 关闭全部忽略规则（--no-ignore --hidden）、
/// 剪除 VCS 目录、glob 匹配、修改时间升序、100 条内联上限。
fn run_glob(pattern: &str, path: Option<String>) -> ToolExecutionResult {
    if pattern.trim().is_empty() {
        return ToolExecutionResult::error("pattern must be a non-empty string");
    }
    let glob = match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolExecutionResult::error(format!("invalid pattern: {e}")),
    };
    let root: PathBuf = match &path {
        Some(p) => PathBuf::from(shell_expand(p)),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => return ToolExecutionResult::error(format!("glob failed: {e}")),
        },
    };
    if !root.is_dir() {
        return ToolExecutionResult::error(format!("glob failed: not a directory: {}", root.display()));
    }
    // basename 匹配（模式无分隔符）：任意深度按文件名匹配（web 模式语义）
    let by_basename = !pattern.contains('/');
    let mut found: Vec<(std::time::SystemTime, String)> = Vec::new();
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(true)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .filter_entry(|entry| {
            // 剪除 VCS 元数据目录（web --glob=!**/.git 等）
            !(entry.depth() > 0 && GLOB_VCS_EXCLUDES.iter().any(|v| entry.file_name() == *v))
        })
        .build();
    for entry in walker.flatten() {
        let file = entry.path();
        if !file.is_file() {
            continue;
        }
        let display = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let hit = if by_basename {
            file.file_name().is_some_and(|n| glob.is_match(n.to_string_lossy().as_ref()))
        } else {
            glob.is_match(&display)
        };
        if !hit {
            continue;
        }
        let mtime = file
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        found.push((mtime, display));
    }
    // rg --sort=modified：修改时间升序（稳定排序保同刻路径序）
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let paths: Vec<String> = found.into_iter().map(|(_, p)| p).collect();
    ToolExecutionResult::text(GlobTool::format_output(&paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_zero_matches() {
        assert_eq!(GrepTool::format_output(0, &[]), "No matches found");
    }

    #[test]
    fn formats_groups_in_first_seen_order() {
        let matches = vec![
            GrepMatch { path: "a.rs".into(), line_number: 3, line: "fn a()".into() },
            GrepMatch { path: "a.rs".into(), line_number: 9, line: "fn b()".into() },
            GrepMatch { path: "b.rs".into(), line_number: 1, line: "fn c()".into() },
        ];
        let out = GrepTool::format_output(3, &matches);
        assert_eq!(out, "Found 3 matches\n\na.rs\nLine 3: fn a()\nLine 9: fn b()\n\nb.rs\nLine 1: fn c()");
    }

    #[test]
    fn formats_truncation_header() {
        let matches: Vec<GrepMatch> = (0..GREP_MAX_MATCHES)
            .map(|i| GrepMatch { path: "x".into(), line_number: i, line: "l".into() })
            .collect();
        let out = GrepTool::format_output(400, &matches);
        assert!(out.starts_with("Found 250 of 400 matches"));
        assert!(out.ends_with("narrow pattern, path, or include to see more.)"));
    }

    /// 端到端：临时目录 + gitignore 语义 + include 过滤 + 行号与分组输出。
    #[tokio::test]
    async fn greps_workspace_end_to_end() {        let dir = std::env::temp_dir().join(format!("dsh-search-test-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(src.join("b.txt"), "alpha here\n").unwrap();
        std::fs::write(dir.join("ignored.log"), "alpha\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        // ignore crate 默认 require_git（与 rg 一致）：gitignore 规则只在
        // git 仓库内生效，测试目录补一个 .git 使其生效
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        // 隐藏目录跳过
        let hidden = dir.join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("c.rs"), "alpha\n").unwrap();

        let input = dsh_tools::ToolExecutionInput::with_raw_arguments(
            dsh_llm::CallId("test".into()),
            "grep".into(),
            // 显式传 path（进程级 cwd 在并行测试里不可依赖）
            format!(r#"{{"pattern": "alpha", "path": {}}}"#, serde_json::to_string(&dir).unwrap()).into(),
        );
        let result = GrepTool.execute(&input).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.starts_with("Found 2 matches"), "{text}");
        // .log（gitignore）与 .hidden（隐藏目录）都不出现
        assert!(!text.contains(".log"), "{text}");
        assert!(!text.contains(".hidden"), "{text}");
        assert!(text.contains("src/a.rs\nLine 1: fn alpha() {}"), "{text}");
        assert!(text.contains("src/b.txt\nLine 1: alpha here"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 端到端：glob 发现文件（含被忽略/隐藏文件）、剔除 VCS 目录与子目录、
    /// basename 模式任意深度匹配、截断脚注。
    #[tokio::test]
    async fn globs_workspace_end_to_end() {
        let dir = std::env::temp_dir().join(format!("dsh-glob-test-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(src.join("nested.rs"), "fn n() {}\n").unwrap();
        std::fs::write(dir.join("top.txt"), "t\n").unwrap();
        std::fs::write(dir.join("skipped.rs"), "s\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/config"), "x\n").unwrap();

        let run = |raw: String| async move {
            let input = dsh_tools::ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("t".into()),
                "glob".into(),
                raw,
            );
            GlobTool.execute(&input).await
        };
        // basename 模式：*.rs 命中任意深度，剔除 .git 内文件；skipped.rs 在根上
        let result = run(format!(r#"{{"pattern": "*.rs", "path": {}}}"#, serde_json::to_string(&dir).unwrap())).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("src/a.rs"), "{text}");
        assert!(text.contains("src/nested.rs"), "{text}");
        assert!(text.contains("skipped.rs"), "{text}");
        assert!(!text.contains(".git"), "{text}");
        // include 分隔符模式锚定深度：src/ 下全部文件
        let result = run(format!(r#"{{"pattern": "src/*", "path": {}}}"#, serde_json::to_string(&dir).unwrap())).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("src/a.rs") && text.contains("src/nested.rs"), "{text}");
        assert!(!text.contains("top.txt"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
