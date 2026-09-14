//! dsh-gpui 库靶：轨迹台账纯逻辑（行模型/分组/折叠/行高几何）与消息块
//! 模型——从 bin 靶抽出以便集成测试直接驱动（bin 靶宏展开深度病态，
//! #[test] 无法在其中展开）。

/// 工具调用块（web 版 ToolRow）。
#[derive(Clone)]
pub struct ToolBlock {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub error: bool,
    pub open: bool,
    /// 读取/差异/搜索卡的 8 行折叠展开态（web 每实例 useState 的对应物）
    pub expanded: bool,
    /// 搜索卡里被折叠的文件组下标（升序；web collapsed Set 的对应物）
    pub collapsed_groups: Vec<usize>,
    /// 调用时长（调用所在 message → tool/result；轨迹台账时间列工具行）
    pub duration_ms: Option<u64>,
}

/// 台账行高规格（上游 TrajectoryCell/TrajectoryTurnHeader module.css 实值）。
pub const TRAJ_CELL_PX: f32 = 38.0;
pub const TRAJ_HEADER_PX: f32 = 44.0;
/// 轮头条内容道最大宽（上游 .inner max-width: 880px）。
pub const TRAJ_LANE_MAX_PX: f32 = 880.0;
/// 轮体规格（上游 TrajectoryTurn .body：gap 10、padding 8/16/22）。
pub const TRAJ_CELL_GAP_PX: f32 = 10.0;
pub const TRAJ_BODY_PAD_T_PX: f32 = 8.0;
pub const TRAJ_BODY_PAD_B_PX: f32 = 22.0;
/// 折叠摘要行高（上游 collapsed-summary td 20px）。
pub const TRAJ_SUMMARY_PX: f32 = 20.0;
/// 组头行高（上游 TrajectoryGroupHeader .root 36px）。
pub const TRAJ_GROUP_PX: f32 = 36.0;

/// 轨迹台账 cell（上游 TrajectoryCellProps 的 rustdsh 子集）。
pub struct TrajCell {
    pub turn: u64,
    pub kind: TrajKind,
    /// 全局序号（上游 #index，1 起）。
    pub index: usize,
    pub text: String,
    /// 回退摘要行（「仅工具调用」等），三级色渲染（上游 .toolCallOnly）。
    pub dim: bool,
    /// 轮内步号（步骤组分组键；user 系与工具行为 None）
    pub step: Option<u64>,
    /// 详情面板用原文（保留换行；text 是单行省略版）。
    pub detail_text: String,
    /// 思考块拼接（消息 cell 详情「思考」节）。
    pub reasoning: Option<String>,
    /// 消息行 usage 三指标（输入/输出/思考）。
    pub metrics: Option<(u64, u64, Option<u64>)>,
    /// 时长列（消息行 = 轮 llm 用时；rustdsh 日志无事件级时间戳，工具行恒 —）。
    pub time_ms: Option<u64>,
    pub tool: Option<ToolBlock>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TrajKind {
    User,
    Message,
    Tool,
}

/// 台账平坦行（uniform_list 虚拟化 item）：轮头条或 cell（cells 下标）。
#[derive(Clone)]
pub enum TrajRow {
    Header(u64),
    Cell(usize),
    /// Message / 步骤 N 组头（上游 TrajectoryGroupHeader，36px）
    Group(String, String),
    /// 折叠轮摘要行（上游 collapsedSummary turn，20px）
    TurnSummary(u64, String),
    /// 折叠助手摘要行（上游 collapsedSummary assistant，20px；携带消息 cell 序号）
    AssistantSummary(usize, String),
}

pub fn traj_layout(
    cells: &[TrajCell],
    collapsed_turns: &std::collections::HashSet<u64>,
    collapsed_assistants: &std::collections::HashSet<usize>,
) -> (Vec<TrajRow>, Vec<f32>) {
        let mut rows: Vec<TrajRow> = Vec::new();
        let mut tops: Vec<f32> = Vec::new();
        let mut y = 0.0f32;
        let mut last_turn: Option<u64> = None;
        let mut ci = 0usize;
        while ci < cells.len() {
            let c = &cells[ci];
            if last_turn != Some(c.turn) {
                rows.push(TrajRow::Header(c.turn));
                tops.push(y);
                y += TRAJ_HEADER_PX + TRAJ_BODY_PAD_T_PX;
                last_turn = Some(c.turn);
                // 折叠轮：整轮换一条 20px 摘要行
                if collapsed_turns.contains(&c.turn) {
                    let turn_cells = cells[ci..].iter().take_while(|n| n.turn == c.turn).count();
                    if turn_cells > 1 {
                        rows.push(TrajRow::TurnSummary(c.turn, c.text.clone()));
                        tops.push(y);
                        y += TRAJ_SUMMARY_PX + TRAJ_BODY_PAD_B_PX;
                        ci += turn_cells;
                        continue;
                    }
                }
            }
            // --- 组头（上游 Message / 步骤 N 组）：user 系 cell 归消息组
            // （连续合并）；带步号的消息 cell 连同其后工具行归步骤组 ---
            let is_msg_group_head =
                c.kind == TrajKind::User || (c.kind == TrajKind::Message && c.step.is_none());
            let is_step_head = c.kind == TrajKind::Message && c.step.is_some();
            let group_len = if is_step_head {
                1 + cells[ci + 1..]
                    .iter()
                    .take_while(|n| n.turn == c.turn && n.kind == TrajKind::Tool)
                    .count()
            } else if is_msg_group_head {
                cells[ci..]
                    .iter()
                    .take_while(|n| {
                        n.turn == c.turn
                            && (n.kind == TrajKind::User
                                || (n.kind == TrajKind::Message && n.step.is_none()))
                    })
                    .count()
                    .max(1)
            } else {
                // 孤儿工具行（理论上不出现）：单独成组
                1
            };
            let group_cells = &cells[ci..ci + group_len];
            let desc_ms: u64 = group_cells.iter().filter_map(|n| n.time_ms).sum();
            let title: String = if is_step_head {
                format!("步骤 {}", c.step.unwrap_or(1))
            } else {
                "消息".to_string()
            };
            rows.push(TrajRow::Group(
                title,
                if desc_ms > 0 {
                    format!(
                        "{} 毫秒",
                        desc_ms
                            .to_string()
                            .as_bytes()
                            .rchunks(3)
                            .rev()
                            .map(|b| std::str::from_utf8(b).unwrap_or_default())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                } else {
                    String::new()
                },
            ));
            tops.push(y);
            y += TRAJ_GROUP_PX + TRAJ_CELL_GAP_PX;
            // 组内行：助手折叠时消息行 + 其后工具行 → 单条摘要行
            let mut gi = 0usize;
            while gi < group_len {
                let gc = &cells[ci + gi];
                let tool_run = cells[ci + gi..]
                    .iter()
                    .skip(1)
                    .take_while(|n| n.kind == TrajKind::Tool)
                    .count();
                if gc.kind == TrajKind::Message
                    && tool_run > 0
                    && collapsed_assistants.contains(&gc.index)
                {
                    let last_in_turn = cells
                        .get(ci + gi + tool_run + 1)
                        .map(|n| n.turn != gc.turn)
                        .unwrap_or(true);
                    rows.push(TrajRow::AssistantSummary(gc.index, gc.text.clone()));
                    tops.push(y);
                    y += TRAJ_SUMMARY_PX
                        + if last_in_turn {
                            TRAJ_BODY_PAD_B_PX
                        } else {
                            TRAJ_CELL_GAP_PX
                        };
                    gi += tool_run + 1;
                    continue;
                }
                rows.push(TrajRow::Cell(ci + gi));
                tops.push(y);
                let last_in_turn = cells
                    .get(ci + gi + 1)
                    .map(|n| n.turn != gc.turn)
                    .unwrap_or(true);
                y += TRAJ_CELL_PX
                    + if last_in_turn {
                        TRAJ_BODY_PAD_B_PX
                    } else {
                        TRAJ_CELL_GAP_PX
                    };
                gi += 1;
            }
            ci += group_len;
        }
        (rows, tops)
    }

/// 会话显示态与 agent 运行态的分离模型（运行中「查看式切换」）。
///
/// 背景：agent 驱动只持有一个会话槽（`set_session` 原地替换），运行中
/// 切换会让进行中的轮次滑到新会话、事件写错文件——原实现因此整段禁止
/// 切换。本模型把「视图指向」与「agent 运行」拆开：运行中允许视图指向
/// 其它会话（peek），但
/// - 运行会话的事件不进视图（照常落盘，切回时从日志重载）；
/// - composer 让位给状态条（异会话期间不可发送/命令，避免写到运行会话）；
/// - 轮次边界后收敛：agent 切到视图会话（保持"空闲时视图=agent"不变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewSplit {
    /// agent 是否有轮次在跑（轮次边界事件驱动，与视图 running 无关）。
    pub agent_busy: bool,
    /// 视图是否指向了非 agent 会话（仅运行中可能出现）。
    pub peeking: bool,
}

impl ViewSplit {
    /// 事件是否进视图（显示路由）：peek 中丢弃运行会话的事件。
    pub fn routes_to_view(&self) -> bool {
        !self.peeking
    }

    /// composer 是否处于「异会话运行中」让位态。
    pub fn composer_inert(&self) -> bool {
        self.agent_busy && self.peeking
    }

    /// 是否该把 agent 收敛到视图会话（轮终 / 发送前的安全网）。
    pub fn needs_commit(&self) -> bool {
        self.peeking && !self.agent_busy
    }

    pub fn on_turn_start(&mut self) {
        self.agent_busy = true;
    }

    /// 轮次边界（TurnEnded / Error）：返回是否需要收敛到视图会话。
    pub fn on_turn_end(&mut self) -> bool {
        self.agent_busy = false;
        self.needs_commit()
    }

    /// 视图切到非 agent 会话。
    pub fn peek(&mut self) {
        self.peeking = true;
    }

    /// 视图回到 agent 会话（或收敛完成）。
    pub fn settle(&mut self) {
        self.peeking = false;
    }
}


// --- 工作区文件树 + 文本预览（上游 ui-sidebar-files / ui-sidebar-documentpreview） ---

/// 目录条目类型（上游 WorkspaceDirectoryEntry.type）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    File,
    Directory,
    Other,
}

/// 目录条目（上游 WorkspaceDirectoryEntry：name/type + 文件字节数可选）。
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: DirEntryKind,
    pub size: Option<u64>,
}

/// 一层目录的内容（上游 DirLevel）。
#[derive(Debug, Clone)]
pub struct DirLevel {
    pub entries: Vec<DirEntry>,
    /// 命中条目上限被截断（条目缺失）。
    pub truncated: bool,
}

/// 目录列表失败（上游 workspace-file/* 代码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesErrorKind {
    NotFound,
    OutsideWorkspace,
    NotDirectory,
    Unavailable,
}

/// 上游 ui-sidebar-files locales 的失败行。
pub fn files_failure_line(kind: FilesErrorKind, message: &str) -> String {
    match kind {
        FilesErrorKind::NotFound => "这个目录不在了。可能已被移动或删除。".into(),
        FilesErrorKind::OutsideWorkspace => "这个目录在工作区之外，侧栏不会读取它。".into(),
        FilesErrorKind::NotDirectory => "这不是一个目录。".into(),
        FilesErrorKind::Unavailable => format!("读取失败：{message}"),
    }
}

/// 预览失败（上游 sidebarDocumentPreview locales 的失败行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewErrorKind {
    NotFound,
    TooLarge { limit: u64 },
    NotText,
    NotRegularFile,
    OutsideWorkspace,
    Unavailable(String),
}

/// 上游 humanBytes：MB/KB 取整。
pub fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

pub fn preview_failure_line(kind: &PreviewErrorKind) -> String {
    match kind {
        PreviewErrorKind::NotFound => "文件不存在，可能已被移动或删除。".into(),
        PreviewErrorKind::TooLarge { limit } => {
            format!("单页内容超过 {} 上限，无法读取。", human_bytes(*limit))
        }
        PreviewErrorKind::NotText => "非文本文件，暂时无法预览。".into(),
        PreviewErrorKind::NotRegularFile => "该路径不是普通文件，没有可显示的内容。".into(),
        PreviewErrorKind::OutsideWorkspace => "这个文件在工作区之外，侧栏不会读取它。".into(),
        PreviewErrorKind::Unavailable(message) => format!("读取失败：{message}"),
    }
}

/// 自然序（上游 Intl.Collator { numeric: true, sensitivity: 'base' } 的子集：
/// 大小写不敏感、数字段按数值比较——file2 在 file10 前）。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn digits_from(s: &[u8], i: usize) -> (u128, usize) {
        let mut n = 0u128;
        let mut j = i;
        while j < s.len() && s[j].is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((s[j] - b'0') as u128);
            j += 1;
        }
        (n, j)
    }
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        if i >= ab.len() && j >= bb.len() {
            return a.cmp(b);
        }
        if i >= ab.len() {
            return std::cmp::Ordering::Less;
        }
        if j >= bb.len() {
            return std::cmp::Ordering::Greater;
        }
        let (ca, cb) = (ab[i], bb[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let (na, ni) = digits_from(ab, i);
            let (nb, nj) = digits_from(bb, j);
            if na != nb {
                return na.cmp(&nb);
            }
            (i, j) = (ni, nj);
            continue;
        }
        let la = (ca as char).to_ascii_lowercase();
        let lb = (cb as char).to_ascii_lowercase();
        if la != lb {
            return la.cmp(&lb);
        }
        i += 1;
        j += 1;
    }
}

/// 上游 orderEntries：目录在前，其余其后，组内自然序。
pub fn order_entries(entries: &[DirEntry]) -> Vec<DirEntry> {
    let mut v: Vec<DirEntry> = entries.to_vec();
    v.sort_by(|left, right| {
        let group = (right.kind == DirEntryKind::Directory)
            .cmp(&(left.kind == DirEntryKind::Directory));
        if group != std::cmp::Ordering::Equal {
            group
        } else {
            natural_cmp(&left.name, &right.name)
        }
    });
    v
}

/// 上游 childPath：父尾分隔符归一后以 `/` 拼接（只作稳定键，不涉真实分隔符）。
pub fn child_path(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches(['/', '\\']))
}

/// 路径是否在工作区根内（分隔符归一 + 边界比较；root 本身算在内）。
pub fn within_root(root: &str, path: &str) -> bool {
    let norm = |s: &str| {
        let mut t = s.replace('\\', "/");
        while t.len() > 1 && t.ends_with('/') {
            t.pop();
        }
        t.to_ascii_lowercase()
    };
    let (r, p) = (norm(root), norm(path));
    p == r || p.starts_with(&format!("{r}/"))
}

/// 目录列表（本地面：上游 workspaceFiles.list 的等价；canonicalize 围栏
/// outside-workspace 覆盖符号链接逃逸）。条目上限 2000（上游 maxEntries 默认）。
pub const DIR_MAX_ENTRIES: usize = 2000;

pub fn list_dir(root: &str, path: &str) -> Result<DirLevel, (FilesErrorKind, String)> {
    let root_canon =
        std::fs::canonicalize(root).map_err(|e| (FilesErrorKind::Unavailable, e.to_string()))?;
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((FilesErrorKind::NotFound, String::new()));
        }
        Err(e) => return Err((FilesErrorKind::Unavailable, e.to_string())),
    };
    let (r, p) = (
        root_canon.to_string_lossy().to_string(),
        canon.to_string_lossy().to_string(),
    );
    if !within_root(&r, &p) {
        return Err((FilesErrorKind::OutsideWorkspace, String::new()));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| (FilesErrorKind::Unavailable, e.to_string()))?;
    if !meta.is_dir() {
        return Err((FilesErrorKind::NotDirectory, String::new()));
    }
    let rd = std::fs::read_dir(&canon).map_err(|e| (FilesErrorKind::Unavailable, e.to_string()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in rd {
        if entries.len() >= DIR_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().to_string();
        let ft = item.file_type().map_err(|e| (FilesErrorKind::Unavailable, e.to_string()))?;
        let (kind, size) = if ft.is_dir() {
            (DirEntryKind::Directory, None)
        } else if ft.is_file() {
            (DirEntryKind::File, item.metadata().ok().map(|m| m.len()))
        } else {
            (DirEntryKind::Other, None)
        };
        entries.push(DirEntry { name, kind, size });
    }
    Ok(DirLevel { entries, truncated })
}

/// 文本分页规格（上游 workspace-files：页行数上限 5000、页字节上限 2MB、
/// 完整读取 32MB 拒绝、NUL 或不可解码 UTF-8 判非文本）。
pub const PAGE_MAX_LINES: usize = 5000;
pub const PAGE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// 一页文本（text = 本页行以 \n 连接；lines = 行数；eof = 已到文件尾）。
#[derive(Debug, Clone)]
pub struct TextPage {
    pub text: String,
    pub lines: usize,
    pub eof: bool,
}

/// 读取一页文本：先按完整文件规格读取（32MB 上限），再切行页。
/// 行页字节超 2MB 判 too-large（上游同语义：页超限整体拒绝，不截断）。
pub fn read_text_page(
    root: &str,
    path: &str,
    offset: usize,
) -> Result<TextPage, PreviewErrorKind> {
    let root_canon = std::fs::canonicalize(root)
        .map_err(|e| PreviewErrorKind::Unavailable(e.to_string()))?;
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PreviewErrorKind::NotFound);
        }
        Err(e) => return Err(PreviewErrorKind::Unavailable(e.to_string())),
    };
    if !within_root(&root_canon.to_string_lossy(), &canon.to_string_lossy()) {
        return Err(PreviewErrorKind::OutsideWorkspace);
    }
    let meta =
        std::fs::metadata(&canon).map_err(|e| PreviewErrorKind::Unavailable(e.to_string()))?;
    if !meta.is_file() {
        return Err(PreviewErrorKind::NotRegularFile);
    }
    if meta.len() > FILE_MAX_BYTES {
        return Err(PreviewErrorKind::TooLarge { limit: FILE_MAX_BYTES });
    }
    let bytes = std::fs::read(&canon).map_err(|e| PreviewErrorKind::Unavailable(e.to_string()))?;
    if bytes.contains(&0) {
        return Err(PreviewErrorKind::NotText);
    }
    let text = String::from_utf8(bytes).map_err(|_| PreviewErrorKind::NotText)?;
    let lines: Vec<&str> = text.split('\n').collect();
    let start = offset.min(lines.len());
    let end = (offset + PAGE_MAX_LINES).min(lines.len());
    let page_lines = &lines[start..end];
    let joined = page_lines.join("\n");
    if joined.len() > PAGE_MAX_BYTES {
        return Err(PreviewErrorKind::TooLarge { limit: PAGE_MAX_BYTES as u64 });
    }
    Ok(TextPage { text: joined, lines: page_lines.len(), eof: end >= lines.len() })
}
