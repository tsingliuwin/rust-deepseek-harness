//! 会话体检：读一份（或扫描全部）session.jsonl.zstd，产出「浪费报告」。
//!
//! 「用-查-改-用」闭环的「查」环节：从原始日志行（含 `time` 毫秒时间戳，
//! 类型化 SessionEntry 不保留）计算每步耗时、按调用粒度定位——
//! ① 归一化后重复执行的命令与重复浪费的时长；
//! ② isError 结果与「错误后紧接同工具重试」链；
//! ③ 结果文本里的 U+FFFD 乱码（Windows cmd GBK stderr 未解码的特征）。
//!
//! 用法：`cargo run -p dsh-persist --example session_doctor -- [path]`
//! path 可为 session.jsonl.zstd 或其所在目录；省略则扫描 ~/.dsh/sessions
//! 全部会话并按浪费时长排序输出一行摘要。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn main() {
    match std::env::args().nth(1) {
        Some(path) if path == "--latest" => {
            // 「用-查-改-用」闭环入口：自动定位最近一次使用的会话
            match latest_session() {
                Some(file) => {
                    if let Err(err) = doctor_one(&file) {
                        eprintln!("failed: {err}");
                        std::process::exit(1);
                    }
                }
                None => {
                    eprintln!("no sessions with recorded activity under {}", sessions_root().display());
                    std::process::exit(1);
                }
            }
        }
        Some(path) => {
            let path = PathBuf::from(path);
            let file = if path.is_dir() { path.join("session.jsonl.zstd") } else { path };
            if let Err(err) = doctor_one(&file) {
                eprintln!("failed: {err}");
                std::process::exit(1);
            }
        }
        None => doctor_all(),
    }
}

/// 按 mtime 倒序找第一个有实际内容（有 step/调用）的会话——只打开没对话
/// 的空会话只有 session 头，没有复查价值。正在进行的对话会持续刷新 mtime。
fn latest_session() -> Option<PathBuf> {
    let root = sessions_root();
    let mut all: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for bucket in std::fs::read_dir(&root).ok()?.flatten() {
        let Ok(sess) = std::fs::read_dir(bucket.path()) else { continue };
        for s in sess.flatten() {
            let f = s.path().join("session.jsonl.zstd");
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            let Ok(modified) = meta.modified() else { continue };
            all.push((modified, f));
        }
    }
    all.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, f) in &all {
        if let Ok(a) = analyze(f) {
            if !a.steps.is_empty() || !a.calls.is_empty() {
                return Some(f.clone());
            }
        }
    }
    None
}

fn sessions_root() -> PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    Path::new(&home).join(".dsh").join("sessions")
}

fn doctor_all() {
    let root = sessions_root();
    let mut found: Vec<PathBuf> = Vec::new();
    // 布局：sessions/<--cwd--bucket>/<session-id>/session.jsonl.zstd
    let Ok(buckets) = std::fs::read_dir(&root) else {
        eprintln!("no sessions dir at {}", root.display());
        std::process::exit(1);
    };
    for bucket in buckets.flatten() {
        let Ok(sess) = std::fs::read_dir(bucket.path()) else { continue };
        for s in sess.flatten() {
            let f = s.path().join("session.jsonl.zstd");
            if f.is_file() {
                found.push(f);
            }
        }
    }
    let mut rows: Vec<(f64, String)> = Vec::new();
    for f in &found {
        match analyze(f) {
            Ok(a) => rows.push((a.wasted_secs(), a.summary_line())),
            Err(_) => rows.push((f64::MAX, format!("LOAD FAILED {}", f.display()))),
        }
    }
    rows.sort_by(|x, y| y.0.total_cmp(&x.0));
    println!("scanned {} sessions (sorted by wasted time):\n", rows.len());
    for (_, line) in &rows {
        println!("{line}");
    }
    println!("\nrun `session_doctor <path-to-session.jsonl.zstd>` for the full report");
}

fn doctor_one(file: &Path) -> Result<(), String> {
    print_report(&analyze(file)?);
    Ok(())
}

// ---------- 单会话分析 ----------

struct StepInfo {
    start_ms: u64,
    end_ms: u64,
    call_idxs: Vec<usize>,
}

struct Call {
    step: u64,
    name: String,
    /// shell 命令剥 `cd` 前缀、空白归一；其它工具为 `op path` 短摘要
    normalized: String,
    time_ms: u64,
    /// 发起 → 对应 tool/result 的毫秒差（秒）
    duration_secs: f64,
}

struct CallResult {
    time_ms: u64,
    is_error: bool,
    text: String,
}

/// 一次请求纪元（request/header）：审计「模型实际看到什么」。
struct HeaderInfo {
    time_ms: u64,
    reason: String,
    provider: String,
    model: String,
    system_chars: usize,
    tools: usize,
}

struct Analysis {
    file: PathBuf,
    cwd: String,
    created_ms: u64,
    turns: u64,
    steps: BTreeMap<(u64, u64), StepInfo>,
    calls: Vec<Call>,
    results: BTreeMap<String, CallResult>,
    out_tokens: u64,
    cache_write: u64,
    cache_read: u64,
    headers: Vec<HeaderInfo>,
}

impl Analysis {
    /// 活跃时长 = 各步耗时之和（会话可能跨天打开，首尾事件差会掺入空闲）。
    fn wall_secs(&self) -> f64 {
        self.steps.values().map(|s| s.end_ms.saturating_sub(s.start_ms)).sum::<u64>() as f64 / 1000.0
    }

    /// 归一化后重复执行的命令里，第 2 次起的时长之和即纯浪费。
    fn wasted_secs(&self) -> f64 {
        let mut groups: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        for c in &self.calls {
            if !c.normalized.is_empty() {
                groups.entry(c.normalized.as_str()).or_default().push(c.duration_secs);
            }
        }
        groups
            .values()
            .filter(|ds| ds.len() > 1)
            .map(|ds| ds.iter().skip(1).sum::<f64>())
            .sum()
    }

    fn error_count(&self) -> usize {
        self.results.values().filter(|r| r.is_error).count()
    }

    /// 首个带时间戳事件的时刻（相对时间的基准）。
    fn first_event_ms(&self) -> u64 {
        self.steps.values().map(|s| s.start_ms).min().unwrap_or(0)
    }

    fn summary_line(&self) -> String {
        let wasted = self.wasted_secs();
        let wasted = if wasted == 0.0 { 0.0 } else { wasted }; // 归一 -0.0
        format!(
            "wasted {:>6.1}s | wall {:>6.1}s | steps {:>3} | calls {:>3} | errors {} | {}",
            wasted,
            self.wall_secs(),
            self.steps.len(),
            self.calls.len(),
            self.error_count(),
            self.file.display()
        )
    }
}

fn analyze(file: &Path) -> Result<Analysis, String> {
    let raw = std::fs::read(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    // 会话文件是多帧 zstd 拼接（逐 chunk 追加各成帧），流式解码天然跨帧
    let mut decoder =
        zstd::stream::Decoder::new(raw.as_slice()).map_err(|e| format!("zstd open {}: {e}", file.display()))?;
    let mut text = String::new();
    decoder.read_to_string(&mut text).map_err(|e| format!("zstd decode {}: {e}", file.display()))?;

    let mut a = Analysis {
        file: file.to_path_buf(),
        cwd: String::new(),
        created_ms: 0,
        turns: 0,
        steps: BTreeMap::new(),
        calls: Vec::new(),
        results: BTreeMap::new(),
        out_tokens: 0,
        cache_write: 0,
        cache_read: 0,
        headers: Vec::new(),
    };
    // call_id → (calls 下标, 发起时间)；结果到达时配对出时长
    let mut open_calls: BTreeMap<String, (usize, u64)> = BTreeMap::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).map_err(|e| format!("bad json line: {e}"))?;
        let t = v.get("time").and_then(|x| x.as_u64()).unwrap_or(0);
        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if typ == "session" {
            // session 行无 data 包装，字段在顶层
            a.cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("").to_string();
            a.created_ms = v.get("createdAt").and_then(|x| x.as_u64()).unwrap_or(0);
            continue;
        }
        let Some(data) = v.get("data") else { continue };
        match typ {
            "turn/start" => {
                a.turns = a.turns.max(data.get("turn").and_then(|x| x.as_u64()).unwrap_or(0));
            }
            "step/start" => {
                let key = step_key(data);
                a.steps.insert(key, StepInfo { start_ms: t, end_ms: t, call_idxs: Vec::new() });
            }
            "step/end" => {
                if let Some(s) = a.steps.get_mut(&step_key(data)) {
                    s.end_ms = t;
                }
            }
            "assistant/message" => {
                let step = data.get("step").and_then(|x| x.as_u64()).unwrap_or(0);
                if let Some(content) = data.pointer("/message/content").and_then(|x| x.as_array()) {
                    for blk in content {
                        if blk.get("type").and_then(|x| x.as_str()) != Some("tool-call") {
                            continue;
                        }
                        let name = blk.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                        let args_raw = blk.get("arguments").and_then(|x| x.as_str()).unwrap_or("");
                        let id = blk.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let idx = a.calls.len();
                        a.calls.push(Call {
                            step,
                            name: name.clone(),
                            normalized: normalize_call(&name, args_raw),
                            time_ms: t,
                            duration_secs: 0.0,
                        });
                        a.steps.entry((a.turns, step)).or_insert(StepInfo { start_ms: t, end_ms: t, call_idxs: Vec::new() }).call_idxs.push(idx);
                        if !id.is_empty() {
                            open_calls.insert(id, (idx, t));
                        }
                    }
                }
                if let Some(u) = data.pointer("/message/usage") {
                    add_usage(&mut a, u);
                }
            }
            "tool/result" => {
                let id = data.get("toolCallId").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let is_error = data.get("isError").and_then(|x| x.as_bool()).unwrap_or(false);
                let text = data
                    .get("content")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
                    })
                    .unwrap_or_default();
                if let Some((idx, start)) = open_calls.remove(&id) {
                    a.calls[idx].duration_secs = t.saturating_sub(start) as f64 / 1000.0;
                }
                a.results.insert(id, CallResult { time_ms: t, is_error, text });
            }
            "assistant/chunk" => {
                if data.pointer("/chunk/type").and_then(|x| x.as_str()) == Some("usage") {
                    if let Some(u) = data.pointer("/chunk/usage") {
                        add_usage(&mut a, u);
                    }
                }
            }
            "request/header" => {
                let reason = data.get("reason").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                let (provider, model) = match data.pointer("/header/config") {
                    Some(c) => (
                        c.get("provider").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                        c.get("model").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                    ),
                    None => ("?".into(), "?".into()),
                };
                let system_chars = data
                    .pointer("/header/system")
                    .and_then(|x| x.as_str())
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                let tools = data.pointer("/header/tools").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
                a.headers.push(HeaderInfo { time_ms: t, reason, provider, model, system_chars, tools });
            }
            _ => {}
        }
    }
    Ok(a)
}

fn step_key(data: &Value) -> (u64, u64) {
    let turn = data.get("turn").and_then(|x| x.as_u64()).unwrap_or(0);
    let step = data.get("step").and_then(|x| x.as_u64()).unwrap_or(0);
    (turn, step)
}

fn add_usage(a: &mut Analysis, u: &Value) {
    a.out_tokens += u.get("outputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
    a.cache_write += u.get("cacheWriteTokens").and_then(|x| x.as_u64()).unwrap_or(0);
    a.cache_read += u.get("cacheReadTokens").and_then(|x| x.as_u64()).unwrap_or(0);
}

/// shell：剥掉 `cd <path> && ` 前缀（一次或多次）、空白归一；
/// fs 等带 op 的工具：`op path`；其余：原样截断。
fn normalize_call(name: &str, args_raw: &str) -> String {
    let Ok(args) = serde_json::from_str::<Value>(args_raw) else { return String::new() };
    if name == "shell" {
        let mut cmd = args.get("command").and_then(|x| x.as_str()).unwrap_or("").to_string();
        loop {
            let trimmed = cmd.trim();
            if let Some(pos) = trimmed.find("&&") {
                let head = trimmed[..pos].trim();
                if head.starts_with("cd ") || head == "cd" {
                    cmd = trimmed[pos + 2..].to_string();
                    continue;
                }
            }
            cmd = trimmed.to_string();
            break;
        }
        // 「换视角重跑」的主体是竖线前的基命令（tail/grep 只是不同切片）
        let base = cmd.split('|').next().unwrap_or(&cmd).trim();
        let base = base.strip_suffix("2>&1").unwrap_or(base).trim();
        return base.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let op = args.get("op").and_then(|x| x.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|x| x.as_str()).unwrap_or("");
    if !op.is_empty() {
        format!("{op} {path}")
    } else {
        args_raw.chars().take(80).collect()
    }
}

fn print_report(a: &Analysis) {
    println!("== session_doctor ==");
    println!("file: {}", a.file.display());
    println!(
        "cwd: {}  created: {}  turns: {}  steps: {}  wall: {:.1}s",
        a.cwd,
        fmt_ms(a.created_ms),
        a.turns,
        a.steps.len(),
        a.wall_secs()
    );
    println!(
        "tool calls: {}  errors: {}  tokens: out {}, cacheWrite {}, cacheRead {}",
        a.calls.len(),
        a.error_count(),
        a.out_tokens,
        a.cache_write,
        a.cache_read
    );

    // 请求纪元：审计 prompt 修复是否随宿主生效的关键凭据
    if let Some(h) = a.headers.last() {
        let rel = h.time_ms.saturating_sub(a.first_event_ms()) as f64 / 1000.0;
        println!(
            "request headers: {}  latest @+{:.1}s: {}/{}  system={:.1}k chars  tools={}  reason={}",
            a.headers.len(),
            rel,
            h.provider,
            h.model,
            h.system_chars as f64 / 1000.0,
            h.tools,
            h.reason
        );
    } else {
        println!("request headers: 0（无审计凭据——宿主可能早于 header 落盘修复，请重建后重试）");
    }

    println!("\n-- steps --");
    for ((turn, step), s) in &a.steps {
        let names =
            s.call_idxs.iter().map(|i| a.calls[*i].name.as_str()).collect::<Vec<_>>().join(",");
        let wall = s.end_ms.saturating_sub(s.start_ms) as f64 / 1000.0;
        let label = if a.turns > 1 { format!("t{turn}s{step}") } else { step.to_string() };
        println!("{:>6} {:>7.1}s  {}", label, wall, if names.is_empty() { "-" } else { &names });
    }

    println!("\n-- 重复执行的命令（第 2 次起即浪费）--");
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, c) in a.calls.iter().enumerate() {
        if !c.normalized.is_empty() {
            groups.entry(c.normalized.as_str()).or_default().push(i);
        }
    }
    let mut repeats = 0;
    for (cmd, idxs) in &groups {
        if idxs.len() < 2 {
            continue;
        }
        let wasted: f64 = idxs.iter().skip(1).map(|i| a.calls[*i].duration_secs).sum();
        if wasted <= 0.0 {
            continue;
        }
        let total: f64 = idxs.iter().map(|i| a.calls[*i].duration_secs).sum();
        repeats += 1;
        println!("[{cmd}]  ×{}  共 {:.1}s，重复浪费 {:.1}s", idxs.len(), total, wasted);
        for i in idxs {
            println!("    step {:>3}  {:>6.1}s", a.calls[*i].step, a.calls[*i].duration_secs);
        }
    }
    if repeats == 0 {
        println!("(无)");
    }

    println!("\n-- 错误 --");
    // result.time 落在哪一步就归到哪一步
    let bounds: Vec<((u64, u64), u64, u64)> =
        a.steps.iter().map(|(k, s)| (*k, s.start_ms, s.end_ms)).collect();
    let mut printed = 0;
    for r in a.results.values().filter(|r| r.is_error) {
        let step = bounds
            .iter()
            .find(|(_, s, e)| r.time_ms >= *s && r.time_ms <= *e)
            .map(|(k, _, _)| k.1.to_string())
            .unwrap_or_else(|| "?".into());
        let snippet: String = r.text.chars().take(120).collect();
        let snippet = if snippet.trim().is_empty() { "(empty)".to_string() } else { snippet.replace('\n', " | ") };
        let moji = if r.text.contains('\u{FFFD}') { "  << 乱码(U+FFFD)" } else { "" };
        println!("step {:>3}  {snippet}{moji}", step);
        printed += 1;
    }
    if printed == 0 {
        println!("(无)");
    }

    println!("\n-- 错误后紧接同工具重试 --");
    let mut retries = 0;
    for w in a.calls.windows(2) {
        let (prev, next) = (&w[0], &w[1]);
        let prev_errored = a
            .results
            .values()
            .any(|r| r.is_error && r.time_ms >= prev.time_ms && r.time_ms < next.time_ms);
        if prev_errored && next.name == prev.name {
            println!("step {:>3} → {:>3}  {}", prev.step, next.step, next.name);
            retries += 1;
        }
    }
    if retries == 0 {
        println!("(无)");
    }

    if a.wasted_secs() > 0.0 {
        println!("\n提示：重复命令多因「想换视角看输出却重跑」。长命令建议先重定向到临时文件再检视。");
    }
}

/// epoch 毫秒 → UTC 文本（避免引入 chrono）。
fn fmt_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = secs / 86400;
    let rem = secs % 86400;
    // civil_from_days（Howard Hinnant 算法）
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}
