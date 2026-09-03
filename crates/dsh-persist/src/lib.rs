//! dsh-persist — 与 web 版 dsh 完全共享的会话持久化。
//!
//! 磁盘布局（web `session-persistence-jsonl` 同款）：
//! `{root}/--{projectKey(cwd)}--/{session-id}/session.jsonl.zstd`
//! - 首行 header：`{"type":"session","version":0,"id",…,"cwd","delegationDepth"}`
//! - 事件行信封：`{"type":"<event>","seq":N,"time":ms,"data":{…}}`
//! 读取时转换为本仓库 `SessionEvent` 词汇表；写入时同样转回 web 信封，
//! 两端互读互通。旧的散文件 `{root}/{id}.jsonl` 仍可读（迁移兼容）。

use dsh_llm::{CallId, ContentBlock, Message, MessageId, MessageSource, SessionId};
use dsh_session::{EpochHeader, HeaderReason, Session, SessionEvent};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// web `projectKey(cwd)`：`/`、`\`、`:` 连续合并为 `-`；ASCII 安全字符
/// （字母数字 `._-`，非 `~`）保留；其余按 UTF-16 code unit 转 `~XXXX`
/// （大写 4 位 hex）；去前导 `-`；`--…--` 包裹；251 截断。
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut sep_run = false;
    for ch in cwd.encode_utf16() {
        let c = char::from_u32(ch as u32).unwrap_or('\u{fffd}');
        if c == '/' || c == '\\' || c == ':' {
            if !sep_run {
                readable.push('-');
            }
            sep_run = true;
        } else if c != '~' && (c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            readable.push(c);
            sep_run = false;
        } else {
            readable.push_str(&format!("~{:04X}", ch));
            sep_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let truncated: String = slug.chars().take(251).collect();
    format!("--{}--", truncated)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// web `projectionCacheDomainSpec`（0.1.2-alpha.5）的当前域版本：per-record
/// 布局，`compatibleVersions: [3, 4]`（v3/v4 行仅缺可选 lineage 字段）。
/// 写侧 stamp 这个值——master 已并入 v6（兼容 3/4/5，行形与 v5 全同），
/// v6 读者经 compatibleVersions 照样接受本 stamp。
pub const PROJCACHE_DOMAIN_VERSION: u64 = 5;

/// 读侧接受的版本戳并集：released v5 兼容 [3,4] ∪ master v6 兼容
/// [3,4,5]。戳在集外（或残缺）的文档按 web `parseRecord` 的 foreign
/// 契约视为 absent，绝不解释。
pub const PROJCACHE_ACCEPTED_VERSIONS: &[u64] = &[3, 4, 5, 6];

/// 读取 web 会话投影缓存的标题表（id → title），双布局：
/// 1) per-record 树 `<dir>/session_projcache/sessions/*.json`（web
///    0.1.2-alpha.5 起的主布局）：信封 `{version, record}`，戳不在
///    [`PROJCACHE_ACCEPTED_VERSIONS`] 内、信封残缺或 `.json.bak.<stamp>`
///    备份文件一律跳过；
/// 2) 旧整档 `<dir>/session_projcache.json`（无树家/旧版 web 的 bootstrap
///    源）：`unit.name`/版本戳验收后按 session 回退。
/// 同 id 两处并存时 per-record 为准（当前布局是权威写面）。
pub fn load_projcache_titles(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let title_of = |record: &serde_json::Value| -> Option<String> {
        record
            .get("rows")
            .and_then(|r| r.get("title"))
            .and_then(|t| t.get("val"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let tree = dir.join("session_projcache").join("sessions");
    if let Ok(entries) = std::fs::read_dir(&tree) {
        for entry in entries.flatten() {
            let path = entry.path();
            // 备份文件（web backupRecord 的 <key>.json.bak.<stamp>）不再以
            // .json 结尾，读侧同款忽略
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let version = doc.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            if !PROJCACHE_ACCEPTED_VERSIONS.contains(&version) {
                continue;
            }
            let Some(record) = doc.get("record").filter(|r| r.is_object()) else {
                continue;
            };
            if let Some(title) = title_of(record)
                && seen.insert(id.to_string())
            {
                out.push((id.to_string(), title));
            }
        }
    }
    let legacy = dir.join("session_projcache.json");
    if let Ok(text) = std::fs::read_to_string(&legacy)
        && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text)
        && doc
            .get("unit")
            .and_then(|u| u.get("name"))
            .and_then(|n| n.as_str())
            == Some("session_projcache")
    {
        let stamped = doc
            .get("unit")
            .and_then(|u| u.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if PROJCACHE_ACCEPTED_VERSIONS.contains(&stamped)
            && let Some(sessions) = doc
                .get("tables")
                .and_then(|t| t.get("sessions"))
                .and_then(|s| s.as_object())
        {
            for (id, row) in sessions {
                if let Some(title) = title_of(row)
                    && seen.insert(id.clone())
                {
                    out.push((id.clone(), title));
                }
            }
        }
    }
    out
}

/// 侧栏列表条目：会话 id + 它的 project cwd（目录名反推不可靠，从 header 读）。
pub struct SessionEntry {
    pub id: SessionId,
    pub cwd: Option<String>,
    pub modified: SystemTime,
}

pub struct SessionRecorder {
    root: PathBuf,
    /// 会话 id → 已解析的会话文件（规范桶锚定；避免 cwd 口径漂移时
    /// 在错误桶里重建/写入副本）。
    resolved: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
    /// 会话 id → 已写到的最大 seq（增量帧追加的 O(1) 序号源；首触时
    /// 由整档解码初始化，单写者约束下可靠）。
    seqs: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl SessionRecorder {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            resolved: std::sync::Mutex::new(Default::default()),
            seqs: std::sync::Mutex::new(Default::default()),
        }
    }

    /// 事件行以独立的 zstd 帧追加到文件尾（zstd 多帧连接是标准格式，
    /// 流式解码跨帧读取）——落盘 O(事件)，替代整档解压+重写。
    fn append_frame(&self, file: &Path, line: &str) -> io::Result<()> {
        use std::io::Write;
        let frame = zstd::stream::encode_all(line.as_bytes(), 0)?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        f.write_all(&frame)
    }

    /// 下一个 seq：缓存优先；首触或缓存缺失时整档解码初始化。
    fn next_seq(&self, id: &SessionId, file: &Path) -> u64 {
        let mut cache = self.seqs.lock().unwrap();
        if let Some(seq) = cache.get(id.as_str()) {
            return *seq + 1;
        }
        let (_, last) = self.read_lines(file);
        cache.insert(id.as_str().to_string(), last);
        last + 1
    }

    /// 规范桶解析：hint 命中优先，其次全桶扫描取最近修改。
    /// 未找到返回 None（调用方决定是否在 hint 桶新建）。
    fn resolve_file(&self, id: &SessionId, hint: Option<&str>) -> Option<PathBuf> {
        let cache = self.resolved.lock().unwrap();
        if let Some(path) = cache.get(id.as_str()) {
            if path.exists() {
                return Some(path.clone());
            }
        }
        drop(cache);
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        let mut hint_hit: Option<PathBuf> = None;
        if let Ok(projs) = fs::read_dir(&self.root) {
            for proj in projs.flatten() {
                let file = proj.path().join(id.as_str()).join("session.jsonl.zstd");
                if !file.exists() {
                    continue;
                }
                if let Some(h) = hint
                    && proj.file_name().to_string_lossy() == project_key(h)
                {
                    hint_hit = Some(file.clone());
                }
                let modified = fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                    best = Some((modified, file));
                }
            }
        }
        let found = hint_hit.or(best.map(|(_, f)| f));
        if let Some(f) = &found {
            self.resolved
                .lock()
                .unwrap()
                .insert(id.as_str().to_string(), f.clone());
        }
        found
    }

    fn project_dir(&self, cwd: &str) -> PathBuf {
        self.root.join(project_key(cwd))
    }

    fn session_file(&self, id: &SessionId, cwd: &str) -> PathBuf {
        self.project_dir(cwd)
            .join(id.as_str())
            .join("session.jsonl.zstd")
    }

    fn legacy_file(&self, id: &SessionId) -> PathBuf {
        self.root.join(format!("{}.jsonl", id.as_str()))
    }

    /// 新建会话：建目录并写 header（web 读取的最小要求）。
    pub fn create(&self, id: &SessionId, cwd: &str, agent_preset: &str) -> io::Result<()> {
        let file = self.session_file(id, cwd);
        // 已存在的日志绝不允许被新 header 覆盖（fail-closed：数据 > 自愈）
        if file.exists() {
            self.resolved
                .lock()
                .unwrap()
                .insert(id.as_str().to_string(), file.clone());
            return Ok(());
        }
        fs::create_dir_all(file.parent().unwrap())?;
        let header = serde_json::json!({
            "type": "session",
            "version": 0,
            "id": id.as_str(),
            "createdAt": now_ms(),
            "cwd": cwd,
            "delegationDepth": 0,
            "agentPreset": agent_preset,
        });
        let mut buf = String::new();
        buf.push_str(&header.to_string());
        buf.push('\n');
        self.append_frame(&file, &buf)?;
        self.resolved
            .lock()
            .unwrap()
            .insert(id.as_str().to_string(), file.clone());
        self.seqs.lock().unwrap().insert(id.as_str().to_string(), 0);
        // projcache 身份行：web 侧栏冷启动先读投影（identity 足够定位）
        self.touch_projcache(id, cwd, 0, None);
        Ok(())
    }

    fn projcache_path(&self) -> PathBuf {
        self.root
            .parent()
            .unwrap_or(&self.root)
            .join("storages")
            .join("session_projcache.json")
    }

    /// per-record 单会话文档路径（web 0.1.2-alpha.5 起的投影缓存主布局）。
    /// key 即路径段，沿用 web `SAFE_KEY_RE`（`[A-Za-z0-9_-]+`）——不安全的
    /// id 只走旧整档面。
    fn projcache_record_path(&self, id: &SessionId) -> Option<PathBuf> {
        if id.as_str().is_empty()
            || !id
                .as_str()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        Some(
            self.root
                .parent()
                .unwrap_or(&self.root)
                .join("storages")
                .join("session_projcache")
                .join("sessions")
                .join(format!("{}.json", id.as_str())),
        )
    }

    /// 把会话投影写入 web 投影缓存，按家的现状选布局：
    ///
    /// - per-record 树已存在（`storages/session_projcache/` 目录在）：写
    ///   主布局单会话文档 `…/sessions/<id>.json`（web 0.1.2-alpha.5 起写侧
    ///   只落这里）。信封 `{version, record}`：已有文档带接受版本戳则原样
    ///   保留（不无谓换版），外来戳/残缺信封按空记录新起——web
    ///   `parseRecord` 的 foreign→absent 契约。读-改-写单文档只动
    ///   `identity.cwd` 与 `rows.title`，其余行（sessionStats 等 web 投影
    ///   行）原样保留。
    /// - 树不存在：只写旧整档 `storages/session_projcache.json`（stamp 3，
    ///   在接受集内），**绝不抢先建树**——web 的 bootstrap 是「树缺席时把
    ///   整档记录整体迁移进树」，rustdsh 若先写一个单会话文档就会让
    ///   bootstrap 永久失效，整档里其余会话的缓存行全部丢失。web 首次
    ///   打开域完成迁移后，本函数自然切到 per-record 面。
    /// - 旧整档在两种模式下都持续维护：web 自身从不写它（bootstrap 后
    ///   只读），保留更新可让旧版整档 web 与新布局并存互通。
    ///
    /// `title = None` 只建/保 identity（web SessionTitle 投影行形：
    /// `rows.title = {ver:1, seq, val}`，latest-wins）。identity 不带
    /// lineage 字段（isSeeded/inheritedEventCount）——alpha.5 起按「未播种
    /// 身份」解释：侧栏列表投影精确匹配，播种过的冷读丢弃到冷重建，正确。
    pub fn touch_projcache(&self, id: &SessionId, cwd: &str, seq: u64, title: Option<&str>) {
        let storages = self.root.parent().unwrap_or(&self.root).join("storages");
        let unit_dir = storages.join("session_projcache");
        if unit_dir.exists() {
            if let Some(path) = self.projcache_record_path(id) {
                let (version, mut record) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|doc| {
                        let version = doc.get("version").and_then(|v| v.as_u64())?;
                        let record = doc.get("record").cloned()?;
                        if !PROJCACHE_ACCEPTED_VERSIONS.contains(&version) {
                            return None;
                        }
                        Some((
                            version,
                            if record.is_object() {
                                record
                            } else {
                                serde_json::json!({})
                            },
                        ))
                    })
                    .unwrap_or((PROJCACHE_DOMAIN_VERSION, serde_json::json!({})));
                if let Some(obj) = record.as_object_mut() {
                    let identity = obj
                        .entry("identity".to_string())
                        .or_insert_with(|| serde_json::json!({"createdAt": now_ms(), "cwd": cwd}));
                    if let Some(obj) = identity.as_object_mut() {
                        // cwd 以最近一次写入为准（会话可跨工作区移动）
                        obj.insert("cwd".into(), serde_json::json!(cwd));
                    }
                    if let Some(title) = title {
                        let rows = obj
                            .entry("rows".to_string())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(rows) = rows.as_object_mut() {
                            rows.insert(
                                "title".into(),
                                serde_json::json!({"ver": 1, "seq": seq, "val": title}),
                            );
                        }
                    }
                    if let Ok(text) = serde_json::to_string_pretty(&serde_json::json!({
                        "version": version,
                        "record": record,
                    })) {
                        // web serializeRecord 同款：pretty + 尾换行
                        let mut text = text;
                        text.push('\n');
                        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                        let _ = std::fs::write(&path, text);
                    }
                }
            }
        }
        let path = self.projcache_path();
        let mut doc: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| {
                serde_json::json!({
                    "unit": {"name": "session_projcache", "version": 3},
                    "global": null,
                    "tables": {"sessions": {}}
                })
            });
        let Some(tables) = doc.get_mut("tables").and_then(|t| t.as_object_mut()) else {
            return;
        };
        let sessions = tables
            .entry("sessions".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(sessions) = sessions.as_object_mut() else {
            return;
        };
        let row = sessions
            .entry(id.as_str().to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(row) = row.as_object_mut() else {
            return;
        };
        let identity = row
            .entry("identity".to_string())
            .or_insert_with(|| serde_json::json!({"createdAt": now_ms(), "cwd": cwd}));
        if let Some(obj) = identity.as_object_mut() {
            // cwd 以最近一次写入为准（会话可跨工作区移动）
            obj.insert("cwd".into(), serde_json::json!(cwd));
        }
        if let Some(title) = title {
            let rows = row
                .entry("rows".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(rows) = rows.as_object_mut() {
                rows.insert(
                    "title".into(),
                    serde_json::json!({"ver": 1, "seq": seq, "val": title}),
                );
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(&path, text);
        }
    }

    /// 追加一个事件（web 信封行；读-改-写整文件压缩）。
    pub fn append(&self, id: &SessionId, cwd: &str, event: &SessionEvent) -> io::Result<()> {
        // 规范桶锚定：写入永远落在会话自身的日志上（hint 只在 id 尚无
        // 任何文件时作为新建位置），杜绝 cwd 口径漂移产生跨桶副本
        let file = match self.resolve_file(id, Some(cwd)) {
            Some(f) => f,
            None => {
                self.create(id, cwd, "standard")?;
                self.session_file(id, cwd)
            }
        };
        // 已存在但解不出内容（非空文件）：拒绝追加（可能是不认识的编码/半写状态）
        if !self.resolved.lock().unwrap().contains_key(id.as_str())
            && file.exists()
            && fs::metadata(&file).map(|m| m.len() > 0).unwrap_or(false)
        {
            let probe = self.read_lines(&file);
            if probe.0.is_empty() {
                self.resolved
                    .lock()
                    .unwrap()
                    .insert(id.as_str().to_string(), file.to_path_buf());
            }
        }
        let seq = self.next_seq(id, &file);
        if let SessionEvent::SessionTitle { title } = event {
            self.touch_projcache(id, cwd, seq, Some(title));
        }
        let row = event_to_web_line(event, seq, now_ms());
        match row {
            Some(row) => {
                self.append_frame(&file, &format!("{}\n", row))?;
                self.seqs
                    .lock()
                    .unwrap()
                    .insert(id.as_str().to_string(), seq);
                Ok(())
            }
            // 不落盘的辅助事件：也不消耗 seq（与整档重写语义一致）
            None => Ok(()),
        }
    }

    /// 会话标题（`session/title` 事件的最新值）。
    pub fn title_of(&self, id: &SessionId, cwd: &str) -> Option<String> {
        let file = self
            .resolve_file(id, Some(cwd))
            .unwrap_or_else(|| self.session_file(id, cwd));
        let (lines, _) = self.read_lines(&file);
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("session/title"))
            .filter_map(|o| o.get("data")?.get("title")?.as_str().map(|s| s.to_string()))
            .next_back()
    }

    /// 加载会话：优先 web 布局；返回 (Session, 真实 cwd)。
    /// cwd 传空时自动扫描目录定位。
    pub fn load(
        &self,
        id: &SessionId,
        cwd_hint: Option<&str>,
    ) -> io::Result<(Session, Option<String>)> {
        // 1) 按 hint
        if let Some(cwd) = cwd_hint {
            let file = self.session_file(id, cwd);
            if file.exists() {
                let events = self.read_events(&file)?;
                return Ok((
                    Session::from_events(id.clone(), events),
                    Some(cwd.to_string()),
                ));
            }
        }
        // 2) 扫描所有 project 目录找该 id（多桶副本取最近修改）
        if let Some(file) = self.resolve_file(id, None) {
            let events = self.read_events(&file)?;
            let cwd = read_header_cwd(&file);
            return Ok((Session::from_events(id.clone(), events), cwd));
        }
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                let file = dir.join("session.jsonl.zstd");
                if file.exists() {
                    let cwd = read_header_cwd(&file);
                    let events = self.read_events(&file)?;
                    return Ok((Session::from_events(id.clone(), events), cwd));
                }
            }
        }
        // 3) legacy 散文件
        let legacy = self.legacy_file(id);
        if legacy.exists() {
            let reader = BufReader::new(File::open(&legacy)?);
            let mut events = Vec::new();
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_str::<SessionEvent>(&line) {
                    events.push(ev);
                }
            }
            return Ok((Session::from_events(id.clone(), events), None));
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "session not found"))
    }

    /// 列出全部会话：web 布局（含每会话 header 的 cwd）+ 旧散文件。
    pub fn list(&self) -> io::Result<Vec<SessionEntry>> {
        let mut out = Vec::new();
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    if proj.file_type()?.is_file() {
                        // 旧散文件
                        let name = proj.file_name().to_string_lossy().into_owned();
                        if let Some(stem) = name.strip_suffix(".jsonl") {
                            if !stem.starts_with("session-") {
                                let modified = proj
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .unwrap_or(UNIX_EPOCH);
                                out.push(SessionEntry {
                                    id: SessionId::new(stem.to_string()),
                                    cwd: None,
                                    modified,
                                });
                            }
                        }
                    }
                    continue;
                }
                for sess in fs::read_dir(proj.path())? {
                    let sess = sess?;
                    if !sess.file_type()?.is_dir() {
                        continue;
                    }
                    let name = sess.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("session-") {
                        continue;
                    }
                    let file = sess.path().join("session.jsonl.zstd");
                    if !file.exists() {
                        continue;
                    }
                    let modified = fs::metadata(&file)
                        .and_then(|m| m.modified())
                        .unwrap_or(UNIX_EPOCH);
                    let cwd = read_header_cwd(&file);
                    out.push(SessionEntry {
                        id: SessionId::new(name),
                        cwd,
                        modified,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.modified.cmp(&a.modified));
        // 同 id 多桶副本（历史缺陷可能遗留）：只保留最近修改的一条
        let mut seen = std::collections::HashSet::new();
        out.retain(|e| seen.insert(e.id.as_str().to_string()));
        Ok(out)
    }

    /// 删除会话（web 布局整目录；散文件兜底）。
    pub fn delete(&self, id: &SessionId, cwd: Option<&str>) -> io::Result<()> {
        self.resolved.lock().unwrap().remove(id.as_str());
        self.seqs.lock().unwrap().remove(id.as_str());
        if let Some(cwd) = cwd {
            let dir = self.project_dir(cwd).join(id.as_str());
            if dir.exists() {
                return fs::remove_dir_all(dir);
            }
        }
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                if dir.exists() {
                    return fs::remove_dir_all(dir);
                }
            }
        }
        fs::remove_file(self.legacy_file(id))
    }

    // ---- 内部 ----

    /// 读取并分类一整个日志（ignorable 契约的执行点）：
    /// 未知类型 + 非 ignorable → 拒绝（Err）；其余未知/未映射类型按分类跳过。
    fn read_events(&self, file: &Path) -> io::Result<Vec<SessionEvent>> {
        let (lines, _) = self.read_lines(file);
        let mut events = Vec::new();
        for line in lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            // 日志首行的 SessionHeader 记录（type:"session"）不是事件，
            // 不参与 ignorable 分类
            if v.get("type").and_then(|t| t.as_str()) == Some("session") {
                continue;
            }
            match web_line_to_event(&v) {
                Some(e) => events.push(e),
                None => match classify_unknown(&v) {
                    UnknownPolicy::KnownSkip | UnknownPolicy::IgnorableSkip => {}
                    UnknownPolicy::Refuse => {
                        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
                        let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "session log contains event type \"{ty}\" (seq {seq}) unknown to this harness and not marked ignorable; refusing to interpret the log — it was likely written by a newer harness"
                            ),
                        ));
                    }
                },
            }
        }
        Ok(events)
    }

    fn read_lines(&self, file: &Path) -> (Vec<String>, u64) {
        let Ok(bytes) = read_decompressed(file) else {
            return (Vec::new(), 0);
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut last_seq = 0u64;
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                    if let Some(s) = v.get("seq").and_then(|s| s.as_u64()) {
                        last_seq = last_seq.max(s);
                    }
                }
                l.to_string()
            })
            .collect();
        (lines, last_seq)
    }
}

fn read_decompressed(file: &Path) -> io::Result<Vec<u8>> {
    let f = File::open(file)?;
    let mut dec = zstd::stream::Decoder::new(f)?;
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

fn read_header_cwd(file: &Path) -> Option<String> {
    let bytes = read_decompressed(file).ok()?;
    let first = bytes.split(|&b| b == b'\n').next()?;
    let v: serde_json::Value = serde_json::from_slice(first).ok()?;
    v.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string())
}

// ---- web 行 ↔ SessionEvent ----

fn blocks_from_web(arr: Option<&serde_json::Value>) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let Some(arr) = arr.and_then(|a| a.as_array()) else {
        return out;
    };
    for b in arr {
        let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match ty {
            "text" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::text(t));
                }
            }
            "reasoning" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::reasoning(t));
                }
            }
            "tool-call" => {
                let id = b.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let args = b.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                out.push(ContentBlock::ToolCall {
                    id: CallId(id.to_string()),
                    name: name.to_string(),
                    arguments: args.to_string(),
                });
            }
            "tool-result" => {
                let call = b
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let content = blocks_from_web(b.get("content"));
                let is_error = b.get("isError").and_then(|v| v.as_bool());
                out.push(ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                });
            }
            _ => {}
        }
    }
    out
}

fn blocks_to_web(blocks: &[ContentBlock]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({"type":"text","text":text}),
            ContentBlock::Reasoning { text } => serde_json::json!({"type":"reasoning","text":text}),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => serde_json::json!({
                "type":"tool-call","id":id.0,"name":name,"arguments":arguments
            }),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => serde_json::json!({
                "type":"tool-result","toolCallId":tool_call_id.0,
                "content":blocks_to_web(content),
                "isError":is_error.unwrap_or(false)
            }),
            ContentBlock::Image { .. } => serde_json::json!({"type":"image"}),
        })
        .collect();
    serde_json::Value::Array(arr)
}

/// web 信封行 → 我们的事件（消息/回合类；chunk 与辅助事件跳过——
/// assistant/message 已含最终内容）。
pub fn web_line_to_event(v: &serde_json::Value) -> Option<SessionEvent> {
    let ty = v.get("type")?.as_str()?;
    let data = v.get("data");
    match ty {
        "turn/start" => Some(SessionEvent::TurnStart {
            turn: num(data, "turn"),
        }),
        "turn/end" => Some(SessionEvent::TurnEnd {
            turn: num(data, "turn"),
            reason: data
                .and_then(|d| d.get("reason"))
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .unwrap_or(dsh_session::TurnEndReason::Completed),
        }),
        "step/start" => Some(SessionEvent::StepStart {
            turn: num(data, "turn"),
            step: num(data, "step"),
        }),
        "user/message" => {
            let d = data?;
            let content = blocks_from_web(d.get("content"));
            let mut msg = Message::user(content);
            if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                msg.id = MessageId(id.to_string());
            }
            // web source.kind 开放词汇：user 之外都是生产者注入的上下文
            // （ContextInjectionRow 展示语义），kind/plugin/form/summary/
            // label 原料原样保留
            let src = d.get("source");
            let kind = src
                .and_then(|s| s.get("kind"))
                .and_then(|k| k.as_str())
                .unwrap_or("user");
            msg.source = match kind {
                "user" => MessageSource::User,
                "plugin" => MessageSource::Context {
                    context_kind: kind.to_string(),
                    plugin: src
                        .and_then(|s| s.get("plugin"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    form: src
                        .and_then(|s| s.get("form"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    summary: src
                        .and_then(|s| s.get("summary"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    changes_paths: Vec::new(),
                    reference_labels: Vec::new(),
                    name: None,
                },
                other => MessageSource::Context {
                    context_kind: other.to_string(),
                    plugin: src
                        .and_then(|s| s.get("plugin"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    form: src
                        .and_then(|s| s.get("form"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    summary: src
                        .and_then(|s| s.get("summary"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    changes_paths: src
                        .and_then(|s| s.get("changes"))
                        .and_then(|c| c.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| {
                                    c.get("path").and_then(|p| p.as_str()).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    reference_labels: src
                        .and_then(|s| s.get("references"))
                        .and_then(|c| c.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| {
                                    c.get("label").and_then(|p| p.as_str()).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    name: src
                        .and_then(|s| s.get("name"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
            };
            Some(SessionEvent::UserMessage(msg))
        }
        "assistant/message" => {
            let d = data?;
            let m = d.get("message")?;
            let content = blocks_from_web(m.get("content"));
            let msg = Message::assistant(content, "web", "web");
            Some(SessionEvent::AssistantMessage {
                turn: num(data, "turn"),
                step: num(data, "step"),
                message: msg,
                interrupted: false,
                usage: None,
            })
        }
        "tool/result" => {
            let d = data?;
            let call = d
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let content = blocks_from_web(d.get("content"));
            let is_error = d.get("isError").and_then(|v| v.as_bool());
            let msg = Message::assistant(
                vec![ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                }],
                "web",
                "web",
            );
            Some(SessionEvent::ToolResult {
                turn: 0,
                step: 0,
                message: msg,
            })
        }
        // 我们自己的压缩/步末事件（step/end 落盘保证 seq 1:1 对齐，
        // 压缩的 beforeSeq 依赖它；web 端按未知类型忽略）
        "step/end" => Some(SessionEvent::StepEnd {
            turn: num(data, "turn"),
            step: num(data, "step"),
        }),
        // 会话标题（web SessionTitleEventData；只取 title，latest-wins）
        "session/title" => Some(SessionEvent::SessionTitle {
            title: data?
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "compaction/summary" => Some(SessionEvent::Compaction {
            before_seq: num(data, "beforeSeq"),
            summary: data?
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "llm/retry" => {
            let d = data?;
            let failure: dsh_llm::LlmFailure = d
                .get("failure")
                .and_then(|f| serde_json::from_value(f.clone()).ok())
                .unwrap_or(dsh_llm::LlmFailure {
                    message: String::new(),
                    code: "UNKNOWN".into(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                });
            Some(SessionEvent::LlmRetry {
                retry_id: d
                    .get("retryId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                turn: num(Some(d), "turn"),
                step: num(Some(d), "step"),
                provider: d
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                mode: d
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal")
                    .to_string(),
                policy_key: d
                    .get("policyKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                retry: num(Some(d), "retry") as u32,
                max_retries: d
                    .get("maxRetries")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                delay_ms: num(Some(d), "delayMs"),
                failure,
            })
        }
        "llm/retry-started" => Some(SessionEvent::LlmRetryStarted {
            retry_id: data?
                .get("retryId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            turn: num(data, "turn"),
            step: num(data, "step"),
            retry: num(data, "retry") as u32,
        }),
        // 请求纪元（event_to_web_line 的对称读回；reason 缺省按 Change 解释）
        "request/header" => {
            let d = data?;
            let header: EpochHeader =
                serde_json::from_value(d.get("header").cloned().unwrap_or(serde_json::Value::Null))
                    .ok()?;
            let reason: HeaderReason = d
                .get("reason")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .unwrap_or(HeaderReason::Change);
            Some(SessionEvent::RequestHeader { header, reason })
        }
        _ => None,
    }
}

/// 本构建理解的事件词汇全集（上游 `KNOWN_SESSION_EVENT_TYPES`，0.1.2-alpha.2
/// 抄定；已逐项核对至 0.1.2-alpha.5 无增补）。
///
/// 持久化读取路径按 [`SessionEvent.ignorable`] 契约分类未知类型：不在集合内
/// 且未标 `ignorable` 的事件——多半是更新版本 harness 写的——拒绝解释整份
/// 日志；标了 `ignorable`（纯信息性记录）则跳过。集合内的类型即便本骨架未
/// 映射（approval/hook/team 等 web 宿主事件）也按已知忽略，不触发拒绝。
pub const KNOWN_SESSION_EVENT_TYPES: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// 存储层 chunk 打包记录类型（web `packChunkRuns` 写盘形态，上游读取时经
/// `decodeStorageRecord` 解回 assistant/chunk——构建认识这些记录，不属于
/// 「未知类型」守卫范围）。骨架没有 chunk 回放消费方，按已知跳过。
pub const KNOWN_STORAGE_RECORD_TYPES: &[&str] =
    &["text-chunks", "reasoning-chunks", "tool-call-chunks"];

/// 未知事件类型的分类（上游读取路径的三分支）。
enum UnknownPolicy {
    /// 词汇表内的类型（本骨架未映射）：按已知忽略。
    KnownSkip,
    /// 词汇表外但标了 `ignorable: true`：纯信息记录，安全跳过。
    IgnorableSkip,
    /// 词汇表外且必读：拒绝解释日志（可能来自更新版本 harness）。
    Refuse,
}

fn classify_unknown(v: &serde_json::Value) -> UnknownPolicy {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    if KNOWN_SESSION_EVENT_TYPES.contains(&ty) || KNOWN_STORAGE_RECORD_TYPES.contains(&ty) {
        return UnknownPolicy::KnownSkip;
    }
    if v.get("ignorable").and_then(|i| i.as_bool()) == Some(true) {
        return UnknownPolicy::IgnorableSkip;
    }
    UnknownPolicy::Refuse
}

fn num(data: Option<&serde_json::Value>, key: &str) -> u64 {
    data.and_then(|d| d.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or_default()
}

/// 我们的事件 → web 信封行（None = 不落盘的辅助事件）。
pub fn event_to_web_line(ev: &SessionEvent, seq: u64, time: u64) -> Option<serde_json::Value> {
    let row = |ty: &str, data: serde_json::Value| serde_json::json!({"type": ty, "seq": seq, "time": time, "data": data});
    match ev {
        SessionEvent::TurnStart { turn } => {
            Some(row("turn/start", serde_json::json!({"turn": turn})))
        }
        SessionEvent::TurnEnd { turn, reason } => Some(row(
            "turn/end",
            serde_json::json!({
                "turn": turn,
                "reason": serde_json::to_value(reason).unwrap_or(serde_json::json!({"kind": "completed"})),
            }),
        )),
        SessionEvent::StepStart { turn, step } => Some(row(
            "step/start",
            serde_json::json!({"turn": turn, "step": step}),
        )),
        SessionEvent::UserMessage(m) => {
            // source 按真实词汇序列化（web merge-extensible sum）：
            // user 原样；注入类还原 kind/plugin/form/summary/changes/references
            let source = match &m.source {
                MessageSource::User => serde_json::json!({"kind": "user"}),
                MessageSource::Context {
                    context_kind,
                    plugin,
                    form,
                    summary,
                    changes_paths,
                    reference_labels,
                    name,
                } => {
                    let mut o = serde_json::Map::new();
                    o.insert("kind".into(), serde_json::json!(context_kind));
                    if let Some(p) = plugin {
                        o.insert("plugin".into(), serde_json::json!(p));
                    }
                    if let Some(f) = form {
                        o.insert("form".into(), serde_json::json!(f));
                    }
                    if let Some(x) = summary {
                        o.insert("summary".into(), serde_json::json!(x));
                    }
                    if !changes_paths.is_empty() {
                        o.insert(
                            "changes".into(),
                            serde_json::Value::Array(
                                changes_paths
                                    .iter()
                                    .map(|p| serde_json::json!({"action": "set", "path": p}))
                                    .collect(),
                            ),
                        );
                    }
                    if !reference_labels.is_empty() {
                        o.insert(
                            "references".into(),
                            serde_json::Value::Array(
                                reference_labels
                                    .iter()
                                    .map(|l| serde_json::json!({"label": l}))
                                    .collect(),
                            ),
                        );
                    }
                    if let Some(n) = name {
                        o.insert("name".into(), serde_json::json!(n));
                    }
                    serde_json::Value::Object(o)
                }
                _ => serde_json::json!({"kind": "user"}),
            };
            Some(row(
                "user/message",
                serde_json::json!({
                    "content": blocks_to_web(&m.content),
                    "source": source,
                    "role": "user",
                    "id": m.id.0,
                    "surfaceOp": "append",
                }),
            ))
        }
        SessionEvent::AssistantMessage {
            turn,
            step,
            message,
            ..
        } => Some(row(
            "assistant/message",
            serde_json::json!({
                "turn": turn, "step": step,
                "message": {"role": "assistant", "content": blocks_to_web(&message.content)},
            }),
        )),
        SessionEvent::ToolResult { message, .. } => {
            let block = message.content.iter().find_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => Some((tool_call_id, content, is_error)),
                _ => None,
            })?;
            Some(row(
                "tool/result",
                serde_json::json!({
                    "toolCallId": block.0 .0,
                    "content": blocks_to_web(block.1),
                    "isError": block.2.unwrap_or(false),
                }),
            ))
        }
        SessionEvent::StepEnd { turn, step } => Some(row(
            "step/end",
            serde_json::json!({"turn": turn, "step": step}),
        )),
        SessionEvent::Compaction {
            before_seq,
            summary,
        } => Some(row(
            "compaction/summary",
            serde_json::json!({"beforeSeq": before_seq, "summary": summary}),
        )),
        SessionEvent::SessionTitle { title } => Some(row(
            "session/title",
            serde_json::json!({"title": title, "messageSeqs": [], "source": {"kind": "fallback"}}),
        )),
        // 流式 chunk（web assistant/chunk：载荷即 StreamChunk 的 serde 形状）
        SessionEvent::AssistantChunk { turn, step, chunk } => Some(row(
            "assistant/chunk",
            serde_json::json!({"chunk": serde_json::to_value(chunk).unwrap_or(serde_json::Value::Null), "turn": turn, "step": step}),
        )),
        // 重试链（web llm/retry：normal 模式带 maxRetries，always 模式不带）
        SessionEvent::LlmRetry {
            retry_id,
            turn,
            step,
            provider,
            mode,
            policy_key,
            retry,
            max_retries,
            delay_ms,
            failure,
        } => {
            let mut data = serde_json::json!({
                "retryId": retry_id,
                "turn": turn,
                "step": step,
                "provider": provider,
                "mode": mode,
                "policyKey": policy_key,
                "retry": retry,
                "delayMs": delay_ms,
                "failure": serde_json::to_value(failure).unwrap_or(serde_json::Value::Null),
            });
            if let (Some(max), Some(obj)) = (max_retries, data.as_object_mut()) {
                obj.insert("maxRetries".into(), serde_json::json!(max));
            }
            Some(row("llm/retry", data))
        }
        SessionEvent::LlmRetryStarted {
            retry_id,
            turn,
            step,
            retry,
        } => Some(row(
            "llm/retry-started",
            serde_json::json!({"retryId": retry_id, "turn": turn, "step": step, "retry": retry}),
        )),
        // 请求纪元（web request/header：config + 实际下发的 system/tools +
        // 变更原因）——审计「模型实际看到什么」的唯一凭据，必须落盘；
        // 首次 Initial、此后仅变更时追加，体量有界
        SessionEvent::RequestHeader { header, reason } => Some(row(
            "request/header",
            serde_json::json!({
                "header": serde_json::to_value(header).unwrap_or(serde_json::Value::Null),
                "reason": serde_json::to_value(reason).unwrap_or(serde_json::Value::Null),
            }),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn title_round_trips_to_web_artifacts() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 预置 per-record 树（web 已完成 bootstrap 的家）：写侧落主布局
        std::fs::create_dir_all(
            dir.join("storages")
                .join("session_projcache")
                .join("sessions"),
        )
        .unwrap();
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-test");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "你好".into(),
            },
        )
        .unwrap();

        // 事件可读回（web session/title 行）
        assert_eq!(rec.title_of(&id, "/tmp/ws"), Some("你好".into()));
        // projcache 投影行落盘（web 侧栏冷启动来源）：主布局 per-record 文档
        let pr: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.join("storages")
                    .join("session_projcache")
                    .join("sessions")
                    .join("session-test.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(pr["version"], PROJCACHE_DOMAIN_VERSION);
        assert_eq!(pr["record"]["identity"]["cwd"], "/tmp/ws");
        assert_eq!(pr["record"]["rows"]["title"]["val"], "你好");
        assert_eq!(pr["record"]["rows"]["title"]["ver"], 1);
        assert_eq!(pr["record"]["rows"]["title"]["seq"], 1);
        // 旧整档（无树家的 bootstrap 源）同步更新
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        let row = &pc["tables"]["sessions"]["session-test"];
        assert_eq!(row["identity"]["cwd"], "/tmp/ws");
        assert_eq!(row["rows"]["title"]["val"], "你好");
        assert_eq!(row["rows"]["title"]["ver"], 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touch_projcache_preserves_stamp_and_sibling_rows() {
        // 已存在的 per-record 文档（master web v6 形）：RMW 保留版本戳、
        // identity.createdAt 与兄弟投影行（sessionStats），只动 cwd 与 title
        let dir = std::env::temp_dir().join(format!("dsh-persist-pr-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-keep");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let path = dir
            .join("storages")
            .join("session_projcache")
            .join("sessions")
            .join("session-keep.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&serde_json::json!({
            "version": 6,
            "record": {
                "identity": {"createdAt": 12345, "cwd": "/tmp/old", "isSeeded": true, "inheritedEventCount": 7},
                "rows": {"sessionStats": {"ver": 1, "seq": 42, "val": {"turns": 3}}}
            }
        })).unwrap()).unwrap();

        rec.touch_projcache(&id, "/tmp/ws", 9, Some("新标题"));

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["version"], 6, "accepted stamp must be preserved");
        assert_eq!(doc["record"]["identity"]["createdAt"], 12345);
        assert_eq!(doc["record"]["identity"]["cwd"], "/tmp/ws");
        assert_eq!(
            doc["record"]["identity"]["isSeeded"], true,
            "lineage fields untouched"
        );
        assert_eq!(doc["record"]["rows"]["sessionStats"]["seq"], 42);
        assert_eq!(doc["record"]["rows"]["title"]["val"], "新标题");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touch_projcache_skips_unsafe_record_key() {
        // per-record 契约：key 必须是路径安全段；不安全 id 只走旧整档面
        let dir = std::env::temp_dir().join(format!("dsh-persist-pr-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session/bad\\id");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle { title: "t".into() },
        )
        .unwrap();
        assert!(
            !dir.join("storages")
                .join("session_projcache")
                .join("sessions")
                .exists()
        );
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            pc["tables"]["sessions"]["session/bad\\id"]["rows"]["title"]["val"],
            "t"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touch_projcache_defers_to_bootstrap_until_tree_exists() {
        // 无树老家：只写旧整档（bootstrap 源），绝不抢先建树——树一旦有
        // 任何文档，web 的整档→树迁移就被永久抑制，整档里其余会话的缓存
        // 行全部丢失。web 完成迁移（树出现）后，写侧自动切 per-record。
        let dir = std::env::temp_dir().join(format!("dsh-persist-defer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-defer");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "旧面".into(),
            },
        )
        .unwrap();
        assert!(
            !dir.join("storages").join("session_projcache").exists(),
            "tree must not be created"
        );
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            pc["tables"]["sessions"]["session-defer"]["rows"]["title"]["val"],
            "旧面"
        );

        // web bootstrap 落地（迁移整档行进树）：此后写侧切 per-record 面
        std::fs::create_dir_all(
            dir.join("storages")
                .join("session_projcache")
                .join("sessions"),
        )
        .unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "新面".into(),
            },
        )
        .unwrap();
        let pr: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.join("storages")
                    .join("session_projcache")
                    .join("sessions")
                    .join("session-defer.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(pr["version"], PROJCACHE_DOMAIN_VERSION);
        assert_eq!(pr["record"]["rows"]["title"]["val"], "新面");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_projcache_titles_reads_both_layouts() {
        // 双布局标题兜底：per-record（v5/v6 戳）优先，foreign 戳/.bak 备份/
        // 残缺信封跳过，旧整档按 session 回退且不覆盖树上已有的 id
        let dir = std::env::temp_dir().join(format!("dsh-persist-titles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sessions = dir.join("session_projcache").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let write_doc = |name: &str, doc: serde_json::Value| {
            std::fs::write(
                sessions.join(name),
                serde_json::to_string_pretty(&doc).unwrap() + "\n",
            )
            .unwrap();
        };
        write_doc(
            "session-a.json",
            serde_json::json!({
                "version": 5,
                "record": {"identity": {"createdAt": 1, "cwd": "/w"}, "rows": {"title": {"ver": 1, "seq": 2, "val": "A"}}}
            }),
        );
        write_doc(
            "session-b.json",
            serde_json::json!({
                "version": 6,
                "record": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 2, "val": "B"}}}
            }),
        );
        // foreign 戳：web parseRecord 契约——视为 absent
        write_doc(
            "session-c.json",
            serde_json::json!({
                "version": 99,
                "record": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 2, "val": "C"}}}
            }),
        );
        // backupRecord 挪开的备份：不再以 .json 结尾，读侧忽略
        std::fs::write(
            sessions.join("session-d.json.bak.202609021548"),
            r#"{"version": 5, "record": {"rows": {"title": {"ver": 1, "seq": 2, "val": "D"}}}}"#,
        )
        .unwrap();
        // 残缺信封：跳过
        std::fs::write(sessions.join("session-e.json"), r#"{"record": {}}"#).unwrap();

        // 旧整档：session-a 已在树上（不覆盖），session-f 回退生效
        std::fs::write(
            dir.join("session_projcache.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "unit": {"name": "session_projcache", "version": 3},
                "global": null,
                "tables": {"sessions": {
                    "session-a": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "stale-A"}}},
                    "session-f": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "F"}}}
                }}
            })).unwrap(),
        ).unwrap();

        let titles = load_projcache_titles(&dir);
        let get = |id: &str| titles.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone());
        assert_eq!(get("session-a").as_deref(), Some("A"));
        assert_eq!(get("session-b").as_deref(), Some("B"));
        assert_eq!(get("session-c"), None, "foreign stamp reads as absent");
        assert_eq!(get("session-d"), None, "backup file is ignored");
        assert_eq!(get("session-e"), None, "malformed envelope reads as absent");
        assert_eq!(get("session-f").as_deref(), Some("F"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_projcache_titles_gates_legacy_stamp() {
        // 旧整档也要过版本门（web bootstrap 同款）：stamp 不在接受集内的
        // 整档不解释
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-titles-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session_projcache.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "unit": {"name": "session_projcache", "version": 99},
                "global": null,
                "tables": {"sessions": {
                    "session-x": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "X"}}}
                }}
            })).unwrap(),
        ).unwrap();
        assert!(load_projcache_titles(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn request_header_round_trips_to_disk() {
        // 回归：event_to_web_line 曾把 RequestHeader 落进 `_ => None`，
        // 「模型实际看到的 system/tools/config」从未写盘——会话审计
        // （用-查-改-用的「查」）无从验证 prompt 修复是否生效。
        let dir = std::env::temp_dir().join(format!("dsh-persist-header-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-header");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let header = EpochHeader {
            config: serde_json::from_value(serde_json::json!({
                "provider": "deepseek", "model": "test-model", "maxTokens": 1024
            }))
            .unwrap(),
            system: Some("You are DeepSeek Harness (Rust).".into()),
            tools: None,
        };
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::RequestHeader {
                header,
                reason: HeaderReason::Initial,
            },
        )
        .unwrap();

        // 落盘行形：request/header + header.system + reason=initial
        let file = rec.session_file(&id, "/tmp/ws");
        let raw_bytes = zstd::stream::decode_all(std::fs::File::open(&file).unwrap()).unwrap();
        let raw = String::from_utf8(raw_bytes).unwrap();
        assert!(raw.contains("\"request/header\""), "{raw}");
        assert!(raw.contains("You are DeepSeek Harness (Rust)."), "{raw}");
        assert!(raw.contains("\"initial\""), "{raw}");

        // 读回：typed 事件还原
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        let restored = session
            .request_header()
            .expect("header should survive load");
        assert_eq!(
            restored.system.as_deref(),
            Some("You are DeepSeek Harness (Rust).")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_event_classification_loads_or_refuses() {
        // 回归：桌面端自己落盘的 tool/call、web 存储层的 chunk 打包记录
        // 都是构建已知类型——不得触发 fail-closed 拒绝（曾因此点击会话
        // 无反应：load Err 被静默吞掉）。
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-ignorable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-classes");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let file = rec.session_file(&id, "/tmp/ws");
        let row = |ty: &str, ignorable: bool| {
            let mut o = serde_json::json!({
                "type": ty,
                "seq": 99,
                "time": 1,
                "data": {"toolCallId": "c1", "name": "fs"},
            });
            if ignorable {
                o["ignorable"] = serde_json::json!(true);
            }
            o.to_string()
        };
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("tool/call", false)
            ),
        )
        .unwrap();
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("text-chunks", false)
            ),
        )
        .unwrap();
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("plugin/future-thing", true)
            ),
        )
        .unwrap();
        // 已知未映射 + 存储记录 + ignorable 未知：都能加载（各自跳过）
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        assert!(session.entries().is_empty());

        // 未知且必读：拒绝解释整份日志
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("plugin/required-thing", false)
            ),
        )
        .unwrap();
        assert!(rec.load(&id, Some("/tmp/ws")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn injection_sources_parse_and_round_trip() {
        // web 注入行（agent-instructions + changes[].path）解析为
        // MessageSource::Context 并经 zstd 往返保留
        let line = serde_json::json!({
            "type": "user/message",
            "data": {
                "id": "m1",
                "role": "user",
                "content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}],
                "source": {
                    "kind": "agent-instructions",
                    "form": "instructions",
                    "baseline": true,
                    "changes": [{"action": "set", "path": "packages/AGENTS.md"}]
                },
                "surfaceOp": "append"
            }
        });
        let ev = web_line_to_event(&line).expect("user message event");
        let SessionEvent::UserMessage(m) = &ev else {
            panic!("wrong event")
        };
        let MessageSource::Context {
            context_kind,
            changes_paths,
            form,
            ..
        } = &m.source
        else {
            panic!("expected Context source, got {:?}", m.source)
        };
        assert_eq!(context_kind, "agent-instructions");
        assert_eq!(changes_paths, &vec!["packages/AGENTS.md".to_string()]);
        assert_eq!(form, &Some("instructions".to_string()));

        let dir = std::env::temp_dir().join(format!("dsh-persist-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-ctx");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(&id, "/tmp/ws", &ev).unwrap();
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        let reloaded = session.entries().iter().find_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => Some(m.source.clone()),
            _ => None,
        });
        assert_eq!(reloaded, Some(m.source.clone()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_with_drifted_cwd_stays_in_canonical_bucket() {
        // 缺陷复现防护：会话建在桶 A，之后以桶 B 的 cwd 口径追加——
        // 必须落在桶 A 的原日志上，绝不产生桶 B 副本
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-canonical-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-drift");
        rec.create(&id, "/tmp/ws-a", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws-a",
            &SessionEvent::UserMessage(Message::user_text("real")),
        )
        .unwrap();

        // cwd 口径漂移（应用侧把当前会话误标为另一个工作区）
        rec.append(
            &id,
            "/tmp/ws-b",
            &SessionEvent::SessionTitle { title: "t".into() },
        )
        .unwrap();
        rec.append(
            &id,
            "/tmp/ws-b",
            &SessionEvent::UserMessage(Message::user_text("second")),
        )
        .unwrap();

        let a_file = dir
            .join("sessions")
            .join(project_key("/tmp/ws-a"))
            .join(id.as_str())
            .join("session.jsonl.zstd");
        let b_file = dir
            .join("sessions")
            .join(project_key("/tmp/ws-b"))
            .join(id.as_str())
            .join("session.jsonl.zstd");
        assert!(a_file.exists());
        assert!(!b_file.exists(), "cross-bucket copy created");
        let (session, _) = rec.load(&id, None).unwrap();
        let users: Vec<_> = session
            .entries()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::UserMessage(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2, "both appends landed on the canonical log");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_never_overwrites_existing_log() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-guard");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::UserMessage(Message::user_text("keep me")),
        )
        .unwrap();
        // 重复 create（如自愈路径误入）不得清掉已有日志
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        assert!(session.entries().iter().any(|e| matches!(
            &e.event,
            SessionEvent::UserMessage(m) if m.content.iter().any(|b| matches!(b, dsh_llm::ContentBlock::Text { text } if text == "keep me"))
        )));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_scans_all_project_buckets() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let a = SessionId::new("session-a");
        let b = SessionId::new("session-b");
        rec.create(&a, "/tmp/ws-a", "standard").unwrap();
        rec.create(&b, "/tmp/ws-b", "standard").unwrap();
        let list = rec.list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|e| e.id == a && e.cwd.as_deref() == Some("/tmp/ws-a"))
        );
        assert!(
            list.iter()
                .any(|e| e.id == b && e.cwd.as_deref() == Some("/tmp/ws-b"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use dsh_session::SessionEvent;

    #[test]
    fn turn_end_error_reason_round_trips() {
        // 回归：turn/end 的 reason 曾被硬编码为 "completed"，失败轮次
        // （LLM 余额不足/超时/中止）在日志里永远"成功"——审计与历史
        // 回放都无法区分。真实 reason 必须双向保真。
        let failure = dsh_llm::LlmFailure {
            message: "Insufficient Balance".into(),
            code: "QUOTA".into(),
            status: Some(402),
            provider_retry_after_ms: None,
            request_id: None,
        };
        let event = SessionEvent::TurnEnd {
            turn: 3,
            reason: dsh_session::TurnEndReason::Error { failure },
        };
        let row = event_to_web_line(&event, 9, 5678).expect("turn/end must map");
        assert_eq!(row["data"]["reason"]["kind"], "error", "{row}");
        assert_eq!(row["data"]["reason"]["failure"]["code"], "QUOTA");
        match web_line_to_event(&row).expect("turn/end must parse back") {
            SessionEvent::TurnEnd { turn, reason } => {
                assert_eq!(turn, 3);
                match reason {
                    dsh_session::TurnEndReason::Error { failure } => {
                        assert_eq!(failure.code, "QUOTA");
                        assert_eq!(failure.message, "Insufficient Balance");
                    }
                    other => panic!("expected Error reason, got {other:?}"),
                }
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
        // 旧格式（硬编码 completed）仍可读
        let legacy: serde_json::Value = serde_json::json!({
            "type": "turn/end", "seq": 1, "time": 1, "data": {"turn": 1, "reason": {"kind": "completed"}}
        });
        match web_line_to_event(&legacy).unwrap() {
            SessionEvent::TurnEnd {
                reason: dsh_session::TurnEndReason::Completed,
                ..
            } => {}
            other => panic!("expected Completed for legacy row, got {other:?}"),
        }
    }

    #[test]
    fn assistant_tool_call_blocks_survive_disk_round_trip() {
        // 回归：历史会话回放（gpui rebuild_from_session）依赖 assistant/
        // message 里的 tool-call 块渲染工具轨迹——落盘/读回任一侧丢块，
        // 打开历史会话就只剩用户气泡。
        let message = Message::assistant(
            vec![
                ContentBlock::text("让我看一下。"),
                ContentBlock::ToolCall {
                    id: CallId("call_1".into()),
                    name: "fs".into(),
                    arguments: r#"{"op":"list","path":"E:\x"}"#.into(),
                },
            ],
            "deepseek",
            "test-model",
        );
        let event = SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            message,
            interrupted: false,
            usage: None,
        };
        let row = event_to_web_line(&event, 7, 1234).expect("assistant/message must map");
        assert_eq!(row["type"], "assistant/message");
        let back = web_line_to_event(&row).expect("assistant/message must parse back");
        match back {
            SessionEvent::AssistantMessage { message, .. } => {
                let kinds: Vec<&str> = message
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { .. } => "text",
                        ContentBlock::ToolCall { .. } => "tool-call",
                        _ => "other",
                    })
                    .collect();
                assert_eq!(kinds, vec!["text", "tool-call"]);
                match &message.content[1] {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(id.0, "call_1");
                        assert_eq!(name, "fs");
                        assert!(arguments.contains("\"op\":\"list\""));
                    }
                    other => panic!("expected tool-call, got {other:?}"),
                }
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }
    }
}
