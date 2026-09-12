//! ChatView — 中栏对话/轨迹主体（独立 entity）。
//!
//! 拆分动机：此前所有状态都在 AppView 上，流式 delta 的 notify 让侧栏全行、
//! 详情、弹层逐帧重建。gpui 的 `AnyView::prepaint` 对非 dirty 子视图复用上帧
//! element_state（bounds/content_mask/text_style 未变即跳过 re-render），把
//! 高频失效收进本视图后，delta 只重建这里。
//!
//! 与 AppView 的边界：ChatView 读 AppView 的 `center_width`（布局产物）；
//! 工具行/轨迹行点击经强句柄回写 `selected_tool`/`details_open`。

use crate::layout;
use crate::theme;
use crate::widgets::{self, dot_sep, state_dot, tip};
use crate::{
    AppView, CenterTab, ChatEntry, ContextInfo, MessageDetail, MsgBlock, Role, ToolBlock,
    ToolDetail, TranscriptView, TurnUsage, format_latency_seconds, format_message_clock,
    format_run_duration, format_tokens_compact, format_tokens_exact, format_tps, now_ms,
};
use dsh_session_projection::turn_outline::{
    PROMPT_PREVIEW_LIMIT, RESPONSE_PREVIEW_LIMIT, preview_parts,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::ButtonVariants;
// uniform_list 句柄的 offset() 在该 trait 上（吸顶算式读滚动偏移用）
use gpui_component::scroll::ScrollbarHandle;
use gpui_component::{Icon, IconName, StyledExt};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 单轮折叠派生（web turn-process 节点数据的对应物）。
#[derive(Clone, Copy)]
pub(crate) struct TurnFold {
    first_process: usize,
    answer: usize,
    tools: usize,
    messages: usize,
    subagents: usize,
}

/// 聊天消息内的单个附件（web .attachmentRow 成员）：
/// 图片 = 64×64 tile（多附件行强制 compact；单图大图样式仅上游图片画廊），
/// 文件 = 240×64 卡（r16、0.5px l2 描边、图标 24×28、名称 14/22 w500、
/// meta = 扩展名 + 大小 12/15 tertiary）。
fn render_chat_attachment(
    attach: &crate::ChatAttachment,
    compact: bool,
) -> gpui::AnyElement {
    let _ = compact;
    match attach {
        crate::ChatAttachment::ImageTile { path } => {
            let mut tile = div()
                .flex_none()
                .size(px(64.0))
                .overflow_hidden()
                .rounded(px(16.0))
                .border(px(0.5))
                .border_color(theme::t().border_l2)
                .bg(theme::t().hover);
            if let Some(p) = path {
                tile = tile.child(
                    gpui::img(p.clone())
                        .size(px(64.0))
                        .object_fit(gpui::ObjectFit::Cover),
                );
            }
            tile.into_any_element()
        }
        crate::ChatAttachment::FileCard { name, bytes } => div()
            .flex_none()
            .w(px(240.0))
            .min_h(px(64.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(16.0))
            .border(px(0.5))
            .border_color(theme::t().border_l2)
            .bg(theme::t().surface)
            .child(file_type_icon(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .line_height(px(22.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text)
                            .truncate()
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(15.0))
                            .text_color(theme::t().text_3)
                            .truncate()
                            .child(format!(
                                "{} {}",
                                crate::extension_of(name),
                                crate::file_size_text(*bytes)
                            )),
                    ),
            )
            .into_any_element(),
    }
}

/// 图片附件对象路径：`attachments/v1/objects/<hex[0..2]>/<hex>`
/// （web attachment-local 布局；attachments_root 不可用或 id 非法 → None）
fn attachment_image_object_path(
    root: Option<&std::path::Path>,
    attachment_id: &str,
) -> Option<std::path::PathBuf> {
    let root = root?;
    let hex = attachment_id.strip_prefix("sha256:")?;
    if hex.len() < 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(root.join("objects").join(&hex[..2]).join(hex))
}

/// composer 下方统计 pill 的数据（web StatsPills 的 WindowStats + token
/// 投影聚合；rustdsh 数值为实时窗口累计）。
#[derive(Clone, Default)]
pub(crate) struct SessionStats {
    pub(crate) turns: u64,
    pub(crate) steps: u64,
    pub(crate) llm_ms: u64,
    pub(crate) tool_ms: u64,
    /// TTFT 平均（毫秒；None = 无计时步）。
    pub(crate) ttft_avg_ms: Option<u64>,
    /// 输出速度 tok/s（None = 无解码窗口）。
    pub(crate) tps: Option<f64>,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read: u64,
    pub(crate) cache_write: u64,
}


/// 附件文件类型分类（上游 ui-primitives FileTypeIcon 传统类；48 语言 code
/// 细分收敛为单一 code——语言级 glyph 资产不可得，见偏差表）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileKind {
    Code,
    Excel,
    Html,
    Image,
    Markdown,
    Other,
    Pdf,
    Ppt,
    Video,
    Word,
}

impl FileKind {
    fn asset_stem(self) -> &'static str {
        match self {
            FileKind::Code => "code",
            FileKind::Excel => "excel",
            FileKind::Html => "html",
            FileKind::Image => "image",
            FileKind::Markdown => "markdown",
            FileKind::Other => "other",
            FileKind::Pdf => "pdf",
            FileKind::Ppt => "ppt",
            FileKind::Video => "video",
            FileKind::Word => "word",
        }
    }

    /// 上游 FileTypeIcon.module.css 每类色（设计平台 static token 实值；
    /// image/video 为 css 自定义 violet）。
    fn color(self) -> gpui::Rgba {
        let (r, g, b): (u8, u8, u8) = match self {
            FileKind::Code | FileKind::Html | FileKind::Markdown => (65, 118, 230),
            FileKind::Excel => (34, 197, 94),
            FileKind::Image | FileKind::Video => (139, 118, 246),
            FileKind::Other => (207, 211, 214),
            FileKind::Pdf => (236, 19, 19),
            FileKind::Ppt => (245, 158, 11),
            FileKind::Word => (86, 134, 254),
        };
        gpui::Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }
}

/// 上游 classifyFileType 传统面：文件名规则 → 扩展名规则，大小写不敏感，
/// 未知名回落 other。
pub(crate) fn classify_file_type(name: &str) -> FileKind {
    let lower = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    match lower.as_str() {
        "readme" | "changelog" | "contributing" => return FileKind::Markdown,
        "makefile" | "gnumakefile" | "bsdmakefile" | "dockerfile" | "gemfile" | "guardfile"
        | "package.json" | "package-lock.json" | "npm-shrinkwrap.json"
        | "docker-compose.yaml" | "docker-compose.yml" | "compose.yaml" | "compose.yml"
        | "cmakelists.txt" => return FileKind::Code,
        _ => {}
    }
    if lower.starts_with('.') {
        return match lower.as_str() {
            ".bash_profile" | ".bashrc" | ".profile" | ".zprofile" | ".zshrc" | ".env"
            | ".gitattributes" | ".gitconfig" | ".gitignore" | ".gitmodules" | ".mailmap"
            | ".commit_editmsg" => FileKind::Code,
            _ => FileKind::Other,
        };
    }
    let ext = match lower.rsplit_once('.') {
        Some((_, e)) if !e.is_empty() => e,
        _ => return FileKind::Other,
    };
    match ext {
        "md" | "mdx" | "markdown" => FileKind::Markdown,
        "pdf" => FileKind::Pdf,
        "ppt" | "pptx" | "key" => FileKind::Ppt,
        "doc" | "docx" | "rtf" | "odt" | "pages" => FileKind::Word,
        "xls" | "xlsx" | "xlsm" | "numbers" => FileKind::Excel,
        "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" | "mpg" | "mpeg" => FileKind::Video,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "avif" | "bmp" | "ico" | "tif"
        | "tiff" | "heic" | "heif" => FileKind::Image,
        "html" | "htm" => FileKind::Html,
        "scss" | "sass" | "less" | "vue" | "svelte" | "astro" | "bat" | "cmd" | "csv" | "tsv"
        | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "rs" | "go" | "java" | "kt"
        | "rb" | "php" | "c" | "h" | "cpp" | "hpp" | "cs" | "swift" | "scala" | "sh" | "sql"
        | "json" | "yaml" | "yml" | "toml" | "xml" | "proto" | "graphql" | "zig" | "lua"
        | "pl" | "r" | "dart" | "erl" | "ex" | "hs" | "clj" | "wasm" | "ini" | "cmake" => {
            FileKind::Code
        }
        _ => FileKind::Other,
    }
}

/// 附件卡类型图标（上游 FileTypeIcon 双层：类色文件底 + 白 mark，28 viewBox；
/// other 类无 mark）。单 tint 限制下 fold 与 body 同色（上游白色折角对比损失）。
pub(crate) fn file_type_icon(name: &str) -> Div {
    let kind = classify_file_type(name);
    let stem = kind.asset_stem();
    let mut d = div()
        .flex_none()
        .w(px(24.0))
        .h(px(28.0))
        .relative()
        .child(
            svg()
                .path(SharedString::from(format!("filetype/{stem}-body.svg")))
                .size_full()
                .text_color(kind.color()),
        );
    if kind != FileKind::Other {
        d = d.child(
            svg()
                .path(SharedString::from(format!("filetype/{stem}-mark.svg")))
                .absolute()
                .size_full()
                .text_color(gpui::white()),
        );
    }
    d
}

pub(crate) struct ChatView {
    /// 只读：会话日志回读（工具结果文本）
    agent: Arc<dsh_agent_loop::ReactLoopAgent>,
    /// 附件存储根（`DSH_HOME/attachments/v1`；图片 tile 字节反查）
    attachments_root: Option<std::path::PathBuf>,
    /// 回写宿主：工具行 → 详情面板；内容列宽读取
    app: Entity<AppView>,
    entries: Vec<ChatEntry>,
    running: bool,
    turn_started_at: Option<Instant>,
    pub(crate) tab: CenterTab,
    /// 轨迹 tab 虚拟化列表滚动句柄（Inspect pill 跳转 scroll_to_item）
    traj_ul: UniformListScrollHandle,
    /// 当前（或最近）轮次号（web turn-process 的分组键）
    ui_turn: u64,
    /// 仍打开的轮次（TurnStarted→TurnEnded；打开的轮次不折叠）
    pub(crate) turn_open: Option<u64>,
    /// 手动展开过的轮次（compact 默认折叠；web 的页内存 manual overrides）
    pub(crate) turn_expanded: std::collections::HashSet<u64>,
    /// 轮次过程折叠的本帧缓存（render 时算一次，render_item 逐可见行读取）
    folds_frame: RefCell<Rc<HashMap<u64, TurnFold>>>,
    /// 消息流虚拟列表（可变高、Top 对齐）
    chat_list: ListState,
    /// 列表条目总数（消息 + 状态行）
    chat_items: usize,
    /// 列表当前是否贴底（scroll handler 维护）
    list_bottom: Rc<Cell<bool>>,
    /// 轮次导航栏激活轮（web activeTurn：滚动读位 + 贴底取最新）
    active_turn: Option<u64>,
    /// 导航栏 hover 预览轮（web previewTurn）
    preview_turn: Option<u64>,
    /// 导航梯内部滚动柄（固定行距梯溢出滚动，web .scroller）
    rail_scroll: gpui::UniformListScrollHandle,
    /// 指针在导航栏内：active 跟随暂停（web pointerInsideRef）
    rail_pointer_inside: bool,
    /// 上次 active 跟随目标（跟随只在离屏时下发一次滚动指令）
    rail_follow: Cell<Option<u64>>,
    /// 下一次 sync 强制 reset（会话切换/重建：内容整体替换）
    chat_reset_pending: bool,
    /// 流式文本增长改了条目高度但没知会虚拟列表 → 按陈旧行高排布、行重叠。
    /// 事件路径置位（渲染中不能改列表），由 tick 消费后统一失效重测。
    heights_dirty: Cell<bool>,
    /// 本轮工具调用计时（call_id → 起始时刻）
    tool_starts: HashMap<String, Instant>,
    /// 本轮累计工具耗时（LLM 时间 = 轮用时 − 工具时间）
    turn_tool_time: Duration,
    /// 本轮 token 累计（TurnEnded 冻结进消息 footer 的统计快照）
    turn_usage: TurnUsage,
    /// 本轮首个 token 时刻（TTFT 数据源）
    turn_first_token: Option<Instant>,
    /// 会话级累计 LLM / 工具耗时（TurnEnded 结转；不持久化——重载会话该组
    /// 整体省略，web StatsLine 同语义：无数据的组整段丢弃）
    session_llm_time: Duration,
    session_tool_time: Duration,
    stats_turns: u64,
    stats_tools: u64,
    stats_steps: u64,
    stats_input_tokens: u64,
    stats_output_tokens: u64,
    stats_cache_read: u64,
    stats_cache_write: u64,
    /// 会话级 TTFT 累计与计时步数（web sessionStats 的 ttftMs/ttftSteps）。
    session_ttft_sum_ms: u64,
    session_ttft_steps: u64,
    /// 会话级纯解码时长与同窗口输出 token（TPS 分母；近似 = llm − ttft）。
    session_decode_ms: u64,
    session_decode_tokens: u64,
    /// 轮次统计药丸的路由标签（provider/model；AppView 切路由时同步）
    pub(crate) route_label: String,
    /// display_path 的相对化基准（随 AppView.current_cwd 同步）
    pub(crate) cwd: String,
    /// AppView 设置镜像：transcript_view 影响折叠派生
    pub(crate) transcript_view: TranscriptView,
}

impl ChatView {
    pub(crate) fn new(
        agent: Arc<dsh_agent_loop::ReactLoopAgent>,
        app: Entity<AppView>,
        route_label: String,
        transcript_view: TranscriptView,
        attachments_root: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = Self {
            agent,
            app,
            attachments_root,
            entries: Vec::new(),
            running: false,
            turn_started_at: None,
            tab: CenterTab::Conversation,
            traj_ul: UniformListScrollHandle::new(),
            ui_turn: 0,
            turn_open: None,
            turn_expanded: Default::default(),
            folds_frame: Default::default(),
            // Top 对齐（web 同语义）：内容自顶排布，短会话首条消息在顶部；
            // 溢出后的吸底由 sync_chat_list 的 scroll_to_reveal_item 承担
            chat_list: ListState::new(0, ListAlignment::Top, px(100.0)),
            chat_items: 0,
            list_bottom: Rc::new(Cell::new(true)),
            active_turn: None,
            preview_turn: None,
            rail_scroll: gpui::UniformListScrollHandle::new(),
            rail_pointer_inside: false,
            rail_follow: Cell::new(None),
            chat_reset_pending: false,
            heights_dirty: Cell::new(false),
            tool_starts: Default::default(),
            turn_tool_time: Duration::ZERO,
            turn_usage: TurnUsage::default(),
            turn_first_token: None,
            session_llm_time: Duration::ZERO,
            session_tool_time: Duration::ZERO,
            stats_turns: 0,
            stats_tools: 0,
            stats_steps: 0,
            stats_input_tokens: 0,
            stats_output_tokens: 0,
            stats_cache_read: 0,
            stats_cache_write: 0,
            session_ttft_sum_ms: 0,
            session_ttft_steps: 0,
            session_decode_ms: 0,
            session_decode_tokens: 0,
            route_label,
            cwd: String::new(),
            transcript_view,
        };
        // 贴底状态跟踪 + 激活轮跟踪：滚动事件更新可见范围是否含末尾；
        // 激活轮按 web 规则取「读位条目」（可视区首条）所属轮次，贴底取
        // 最新轮（web turnAtLine / 贴底覆盖的同语义）。
        {
            let list = view.chat_list.clone();
            let bottom = Rc::clone(&view.list_bottom);
            let this = cx.entity().downgrade();
            list.set_scroll_handler(move |ev, _window, cx| {
                // visible_range.end 是后半开区间：末项可见 ⟺ end > last
                let at_bottom = ev.visible_range.end >= ev.count;
                bottom.set(at_bottom);
                let Some(view) = this.upgrade() else { return };
                let read_ix = ev.visible_range.start;
                view.update(cx, |v, cx| {
                    let next = if at_bottom || v.entries.is_empty() {
                        v.entries.last().map(|e| e.turn)
                    } else {
                        v.entries
                            .get(read_ix.min(v.entries.len() - 1))
                            .map(|e| e.turn)
                    };
                    if v.active_turn != next {
                        v.active_turn = next;
                        cx.notify();
                    }
                });
            });
        }
        // 流式期间 33ms tick（≈每帧一次）：heights_dirty 消费（统一重测
        // 行高）+ 状态行耗时跳动。停流后再守 ~264ms 宽限，兜住最后一次
        // 解析发布（TextView 后台解析晚于最后 delta 落地）。delta 级
        // notify 不出本视图。
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            const GRACE_TICKS: u32 = 8;
            let mut cx = cx.clone();
            let mut grace = 0u32;
            loop {
                Timer::after(Duration::from_millis(33)).await;
                let Some(tick) = this.upgrade() else { return; };
                let Ok(running) = tick.update(&mut cx, |v, _| v.running) else {
                    return;
                };
                if running {
                    grace = GRACE_TICKS;
                } else if grace == 0 {
                    continue;
                } else {
                    grace -= 1;
                }
                let Ok(dirty) = tick.update(&mut cx, |v, _| v.heights_dirty.replace(false)) else {
                    return;
                };
                if dirty || !running {
                    let _ = tick.update(&mut cx, |v, _| {
                        v.invalidate_stream_tail();
                        // splice 对 old_range 内的滚动锚会回拽到范围头，
                        // 贴底态须立即重新锚定到底，否则视口跳到该条开头
                        if v.list_bottom.get() {
                            let n = v.chat_item_count();
                            v.chat_list.scroll_to(ListOffset { item_ix: n, offset_in_item: px(0.) });
                        }
                    });
                    let _ = tick.update(&mut cx, |_, cx| cx.notify());
                }
            }
        })
        .detach();
        view
    }

    /// 虚拟列表条目总数：消息 + 流式状态行。
    fn chat_item_count(&self) -> usize {
        self.entries.len() + usize::from(self.running)
    }

    /// 同步列表长度并按需滚底（流式期间沿用贴底语义）。
    fn sync_chat_list(&mut self, force_bottom: bool) {
        let n = self.chat_item_count();
        // append-only 增长（流式）走 splice：保留滚动锚点——reset 会清
        // logical_scroll_top 把视图弹回顶部，吸底跟随随之失效
        if self.chat_reset_pending || n < self.chat_items {
            self.chat_list.reset(n);
            self.chat_reset_pending = false;
        } else if n > self.chat_items {
            // splice 的第二参数是替换后的数量：空范围插 (n - 旧总数) 条
            self.chat_list.splice(self.chat_items..self.chat_items, n - self.chat_items);
        }
        self.chat_items = n;
        if n > 0 && (force_bottom || self.list_bottom.get()) {
            // 贴底锚定用逻辑偏移（item_ix = n = 列表末边界），不依赖各条
            // 已测高度。曾用 scroll_to_reveal_item(n-1)：它按陈旧高度和算
            // 底部位置，流式时最后一条持续长高，目标偏移反复偏差 → 上下闪
            self.chat_list.scroll_to(ListOffset { item_ix: n, offset_in_item: px(0.) });
            // 贴底时最新轮持有导航栏激活标记（web toBottom → setActiveTurn）
            self.active_turn = self.entries.last().map(|e| e.turn);
        }
    }

    /// 条目高度失效（区间化）：内容高度不经 splice/reset 变化（展开收起、
    /// 折叠切换、流式文本增长）时，列表按陈旧行高排布、行间重叠，需把对应
    /// 条目标记 Unmeasured 重测。此前全量 splice(0..n) 除把所有高度清零
    /// 外，还会把 old_range 内的滚动锚拽回范围头（= 列表顶），再被下一个
    /// 流式事件拉回底部——高帧率下表现为周期性上下闪。
    fn invalidate_chat_heights_range(&self, range: std::ops::Range<usize>) {
        let n = self.chat_item_count();
        let end = range.end.min(n);
        if range.start < end {
            self.chat_list.splice(range.start..end, end - range.start);
        }
    }

    /// 流式文本增长的高度失效：只有最后一条消息（与状态行）在长高。
    fn invalidate_stream_tail(&self) {
        let last_entry = self.entries.len().saturating_sub(1);
        let n = self.chat_item_count();
        self.invalidate_chat_heights_range(last_entry..n);
    }

    /// 消息流当前是否贴底（scroll handler 维护的可见范围判断）。
    fn chat_near_bottom(&self) -> bool {
        self.list_bottom.get()
    }

    /// web StatsLine 文本（组内数据缺失时整组省略）。
    pub(crate) fn stats_line(&self) -> String {
        stats_line_text(
            self.stats_turns,
            self.stats_steps,
            self.session_llm_time,
            self.session_tool_time,
            self.stats_input_tokens,
            self.stats_output_tokens,
            self.stats_cache_read,
        )
    }

    /// composer 下方统计 pill 的数据（web StatsPills：窗口 fold fallback 语义，
    /// 持久投影无镜像——数值为实时窗口累计）。
    pub(crate) fn session_stats(&self) -> SessionStats {
        SessionStats {
            turns: self.stats_turns,
            steps: self.stats_steps,
            llm_ms: self.session_llm_time.as_millis() as u64,
            tool_ms: self.session_tool_time.as_millis() as u64,
            ttft_avg_ms: if self.session_ttft_steps > 0 {
                Some(self.session_ttft_sum_ms / self.session_ttft_steps)
            } else {
                None
            },
            tps: if self.session_decode_ms > 0 {
                Some(self.session_decode_tokens as f64 / (self.session_decode_ms as f64 / 1000.0))
            } else {
                None
            },
            input_tokens: self.stats_input_tokens,
            output_tokens: self.stats_output_tokens,
            cache_read: self.stats_cache_read,
            cache_write: self.stats_cache_write,
        }
    }

    pub(crate) fn stats_steps(&self) -> u64 {
        self.stats_steps
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn running(&self) -> bool {
        self.running
    }

    pub(crate) fn tab(&self) -> CenterTab {
        self.tab
    }

    /// 清空至全新会话态（new_session / blank draft / rebind 共用）。
    pub(crate) fn reset_empty(&mut self) {
        self.entries.clear();
        self.running = false;
        self.turn_started_at = None;
        self.stats_turns = 0;
        self.stats_tools = 0;
        self.stats_steps = 0;
        self.stats_input_tokens = 0;
        self.stats_output_tokens = 0;
        self.stats_cache_read = 0;
        self.stats_cache_write = 0;
        self.session_ttft_sum_ms = 0;
        self.session_ttft_steps = 0;
        self.session_decode_ms = 0;
        self.session_decode_tokens = 0;
        self.session_llm_time = Duration::ZERO;
        self.session_tool_time = Duration::ZERO;
        self.turn_tool_time = Duration::ZERO;
        self.tool_starts.clear();
        self.tab = CenterTab::Conversation;
        self.chat_reset_pending = true;
        self.sync_chat_list(true);
    }

    /// Rebuild the transcript from the agent's session log (restore/switch).
    pub(crate) fn rebuild_from_session(&mut self) {
        self.reset_empty();
        self.turn_expanded.clear();
        self.turn_open = None; // 回放态全部视为已关闭（可折叠判定成立）
        let session = self.agent.session();
        let session = session.lock().unwrap();
        let mut cur_turn = 0u64;
        for entry in session.entries() {
            match &entry.event {
                SessionEvent::TurnStart { turn } => {
                    cur_turn = *turn;
                    self.stats_turns += 1;
                }
                SessionEvent::TurnEnd { reason: TurnEndReason::Error { failure }, .. } => {
                    // 失败轮次在历史里也要可见（实时路径由 AgentEvent::Error
                    // 入列；回放此前只有用户气泡、轮次看起来凭空蒸发）
                    self.entries.push(ChatEntry {
                        role: Role::Error,
                        blocks: vec![MsgBlock::Text(format!("[{}] {}", failure.code, failure.message))],
                        done: true,
                        elapsed: None, usage: None, ended_at_ms: None,
                        turn: cur_turn,
                        context: None,
                        open: false,
                    });
                }
                SessionEvent::AssistantMessage { usage, message, .. } => {
                    self.stats_steps += 1;
                    if let Some(u) = &usage {
                        self.stats_input_tokens += u.input_tokens;
                        self.stats_output_tokens += u.output_tokens;
                        if let Some(cr) = u.cache_read_tokens { self.stats_cache_read += cr; }
                        if let Some(cw) = u.cache_write_tokens { self.stats_cache_write += cw; }
                    }
                    // 回放助手内容（文本/推理/工具调用行）。实时路径由 chunk 流
                    // 增量入列，历史回放唯一来源就是这条完整消息——曾缺失此
                    // 分支导致打开历史会话只剩用户气泡、轨迹无工具记录。
                    let mut blocks: Vec<MsgBlock> = Vec::new();
                    for b in &message.content {
                        match b {
                            ContentBlock::Text { text } => {
                                if !text.is_empty() {
                                    blocks.push(MsgBlock::Text(text.clone()));
                                }
                            }
                            ContentBlock::Reasoning { text } => {
                                blocks.push(MsgBlock::Reasoning { text: text.clone(), open: false });
                            }
                            ContentBlock::ToolCall { id, name, arguments } => {
                                self.stats_tools += 1;
                                blocks.push(MsgBlock::Tool(ToolBlock {
                                    id: id.0.clone(),
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                    result: None,
                                    error: false,
                                    open: false,
                                    expanded: false,
                                    collapsed_groups: Vec::new(),
                                }));
                            }
                            _ => {}
                        }
                    }
                    if !blocks.is_empty() {
                        // 轨迹指标列数据源：事件级 usage 落条目快照
                        // （计时字段日志无承载，留 0 → 时长列按缺省 — 渲染）
                        self.entries.push(ChatEntry {
                            role: Role::Assistant,
                            blocks,
                            done: true,
                            elapsed: None,
                            usage: usage.as_ref().map(|u| TurnUsage {
                                input_tokens: u.input_tokens,
                                output_tokens: u.output_tokens,
                                cache_read: u.cache_read_tokens,
                                cache_write: u.cache_write_tokens,
                                reasoning: u.reasoning_tokens,
                                run_ms: 0,
                                llm_ms: 0,
                                ttft_ms: None,
                                route: String::new(),
                            }),
                            ended_at_ms: None,
                            turn: cur_turn,
                            context: None,
                            open: false,
                        });
                    }
                }
                SessionEvent::ToolCall { .. } => { self.stats_tools += 1; }
                // v3 system prompt 面节点（上游 SystemPromptRow）：折叠行
                // 「系统提示词」/「系统提示词更新」（replace 行），展开体为
                // 提示词全文；空内容（dormant head）不显示。
                SessionEvent::SystemMessage { message, replace, .. } => {
                    let text: String = message
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() {
                        continue;
                    }
                    self.entries.push(ChatEntry {
                        role: Role::Context,
                        blocks: vec![MsgBlock::Text(text)],
                        done: true,
                        elapsed: None, usage: None, ended_at_ms: None,
                        turn: cur_turn,
                        context: Some(crate::ContextInfo {
                            title: if replace.is_some() { "系统提示词更新" } else { "系统提示词" },
                            label: None,
                            summary: None,
                        }),
                        open: false,
                    });
                }
                SessionEvent::UserMessage(m) => {
                    let text: String = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    // v2 混合附件：文件卡 / 图片 tile（图片字节按
                    // sha256 反查附件存储 objects/<2hex>/<hex>）
                    let mut attachments: Vec<MsgBlock> = Vec::new();
                    for b in &m.content {
                        match b {
                            ContentBlock::File { attachment } => {
                                attachments.push(MsgBlock::Attachment(
                                    crate::ChatAttachment::FileCard {
                                        name: attachment.name.clone(),
                                        bytes: attachment.bytes,
                                    },
                                ));
                            }
                            ContentBlock::Image { attachment } => {
                                attachments.push(MsgBlock::Attachment(
                                    crate::ChatAttachment::ImageTile {
                                        path: attachment_image_object_path(
                                            self.attachments_root.as_deref(),
                                            &attachment.attachment_id,
                                        ),
                                    },
                                ));
                            }
                            _ => {}
                        }
                    }
                    if text.is_empty() && attachments.is_empty() {
                        continue;
                    }
                    // 生产者注入的上下文 → ContextInjectionRow（非用户气泡）
                    if let MessageSource::Context { context_kind, plugin, form, summary, changes_paths, reference_labels, name } =
                        &m.source
                    {
                        let info = crate::context_info(context_kind, plugin, form, summary, changes_paths, reference_labels, name);
                        self.entries.push(ChatEntry {
                            role: Role::Context,
                            blocks: vec![MsgBlock::Text(text)],
                            done: true,
                            elapsed: None, usage: None, ended_at_ms: None,
                            turn: cur_turn,
                            context: Some(info),
                            open: false,
                        });
                        continue;
                    }
                    let mut blocks = attachments;
                    if !text.is_empty() {
                        blocks.push(MsgBlock::Text(text));
                    }
                    self.entries.push(ChatEntry {
                        role: Role::User,
                        blocks,
                        done: true,
                        elapsed: None, usage: None, ended_at_ms: None,
                        turn: cur_turn,
                        context: None,
                        open: false,
                    });
                }
                SessionEvent::ToolResult { message, .. } => {
                    if let Some(ContentBlock::ToolResult { tool_call_id, content, is_error }) =
                        message.content.first()
                    {
                        let result_text: String = content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        attach_tool_result(self.entries.last_mut(), &tool_call_id.0, &result_text, is_error.unwrap_or(false));
                    }
                }
                SessionEvent::Compaction { .. } => {
                    self.entries.push(ChatEntry { role: Role::Notice, blocks: vec![], done: true, elapsed: None, usage: None, ended_at_ms: None, turn: cur_turn, context: None,
open: false,
});
                }
                _ => {}
            }
        }
        self.ui_turn = cur_turn;
        // 回放后条目总数已变：标记整体替换，下一次 sync reset + 滚底
        self.chat_reset_pending = true;
        self.sync_chat_list(true);
    }

    fn last_assistant(&mut self) -> &mut ChatEntry {
        let new = !matches!(self.entries.last(), Some(e) if e.role == Role::Assistant && !e.done);
        if new {
            self.entries.push(ChatEntry {
                role: Role::Assistant,
                blocks: Vec::new(),
                done: false,
                elapsed: None, usage: None, ended_at_ms: None,
                turn: self.ui_turn,
             context: None,
open: false,
});
        }
        self.entries.last_mut().unwrap()
    }

    fn push_text(&mut self, text: &str) {
        let blocks = &mut self.last_assistant().blocks;
        match blocks.last_mut() {
            Some(MsgBlock::Text(t)) => t.push_str(text),
            _ => blocks.push(MsgBlock::Text(text.to_string())),
        }
        self.heights_dirty.set(true);
    }

    fn push_reasoning(&mut self, text: &str) {
        let blocks = &mut self.last_assistant().blocks;
        match blocks.last_mut() {
            Some(MsgBlock::Reasoning { text: t, .. }) => t.push_str(text),
            _ => blocks.push(MsgBlock::Reasoning { text: text.to_string(), open: false }),
        }
        self.heights_dirty.set(true);
    }

    /// 用户消息入列（标题逻辑在 AppView：sessions/recorder 是宿主职责）。
    /// 用户消息条目（混合附件版）：`text = None` 为附件-only 发送；
    /// 附件块排在文本前（渲染时呈气泡右上区域）。
    pub(crate) fn push_user_entry_with(
        &mut self,
        text: Option<String>,
        attachments: Vec<crate::ChatAttachment>,
    ) {
        let mut blocks: Vec<MsgBlock> =
            attachments.into_iter().map(MsgBlock::Attachment).collect();
        if let Some(t) = text {
            blocks.push(MsgBlock::Text(t));
        }
        self.entries.push(ChatEntry {
            role: Role::User,
            blocks,
            done: true,
            elapsed: None, usage: None, ended_at_ms: None,
            turn: self.ui_turn,
         context: None,
open: false,
});
        self.sync_chat_list(true);
    }

    /// @ 文件引用的上下文注入行入列。
    pub(crate) fn push_context_entry(&mut self, info: ContextInfo, text: String) {
        self.entries.push(ChatEntry {
            role: Role::Context,
            blocks: vec![MsgBlock::Text(text)],
            done: true,
            elapsed: None, usage: None, ended_at_ms: None,
            turn: self.ui_turn,
            context: Some(info),
            open: false,
        });
        self.sync_chat_list(false);
    }

    /// 实时事件入列。返回是否需要宿主级刷新（运行态翻转 / 统计行更新）——
    /// delta 级事件只 notify 本视图。
    pub(crate) fn apply_event(&mut self, ev: AgentEvent) -> bool {
        let app_level = matches!(
            &ev,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnEnded { .. }
                | AgentEvent::Error { .. }
                | AgentEvent::AssistantMessage { .. }
        );
        match ev {
            AgentEvent::TurnStarted { turn } => {
                self.running = true;
                self.turn_started_at = Some(Instant::now());
                self.stats_turns += 1;
                // 用户条目在发送时入列，此刻 ui_turn 还是上一轮（首发送为 0）
                // ——回戳尾随的 user/context/notice 条目到本轮，否则台账裂出
                // 「第 0 轮」独占段、后续发送的用户泡挂到上一轮头下。
                for e in self.entries.iter_mut().rev() {
                    match e.role {
                        Role::User | Role::Context | Role::Notice => {
                            if e.turn >= turn {
                                break;
                            }
                            e.turn = turn;
                        }
                        _ => break,
                    }
                }
                self.ui_turn = turn;
                self.turn_open = Some(turn);
                self.turn_tool_time = Duration::ZERO;
                self.turn_usage = TurnUsage::default();
                self.turn_first_token = None;
                self.tool_starts.clear();
                self.heights_dirty.set(false);
            }
            AgentEvent::TextDelta { text } => {
                if self.turn_first_token.is_none() {
                    self.turn_first_token = Some(Instant::now());
                }
                self.push_text(&text);
            }
            AgentEvent::ReasoningDelta { text } => {
                if self.turn_first_token.is_none() {
                    self.turn_first_token = Some(Instant::now());
                }
                self.push_reasoning(&text);
            }
            AgentEvent::ToolCall { tool_call_id, name, arguments: args } => {
                self.stats_tools += 1;
                self.tool_starts.insert(tool_call_id.0.clone(), Instant::now());
                self.last_assistant().blocks.push(MsgBlock::Tool(ToolBlock {
                    id: tool_call_id.0,
                    name,
                    arguments: args,
                    result: None,
                    error: false,
                    open: false,
                    expanded: false,
                    collapsed_groups: Vec::new(),
                }));
            }
            AgentEvent::ToolResult { tool_call_id, is_error } => {
                if let Some(start) = self.tool_starts.remove(&tool_call_id.0) {
                    self.turn_tool_time += start.elapsed();
                }
                // 实时结果文本从会话日志回读（事件本身只带 id/error）。
                let result = self.latest_tool_result_text(&tool_call_id.0);
                let last = self.entries.last_mut();
                attach_tool_result(last, &tool_call_id.0, result.0.as_str(), is_error);
            }
            AgentEvent::AssistantMessage { usage, .. } => {
                self.stats_steps += 1;
                if let Some(u) = &usage {
                    self.stats_input_tokens += u.input_tokens;
                    self.stats_output_tokens += u.output_tokens;
                    if let Some(cr) = u.cache_read_tokens { self.stats_cache_read += cr; }
                    if let Some(cw) = u.cache_write_tokens { self.stats_cache_write += cw; }
                    // 本轮 token 四桶累计（web TurnTokenUsage 的折叠语义）
                    self.turn_usage.input_tokens += u.input_tokens;
                    self.turn_usage.output_tokens += u.output_tokens;
                    let acc = &mut self.turn_usage;
                    acc.cache_read = match (acc.cache_read, u.cache_read_tokens) {
                        (Some(a), Some(b)) => Some(a + b),
                        (None, b) => b,
                        (a, None) => a,
                    };
                    acc.cache_write = match (acc.cache_write, u.cache_write_tokens) {
                        (Some(a), Some(b)) => Some(a + b),
                        (None, b) => b,
                        (a, None) => a,
                    };
                    acc.reasoning = match (acc.reasoning, u.reasoning_tokens) {
                        (Some(a), Some(b)) => Some(a + b),
                        (None, b) => b,
                        (a, None) => a,
                    };
                }
                // 步结（上游台账步粒度）：本步 usage 冻结进流式条目并 done，
                // 下一步 delta 经 last_assistant 开新条目——实时台账与回放
                // 的每步形态一致（此前整轮折一条、运行中指标列恒空）。
                if let Some(e) = self.entries.last_mut() {
                    if e.role == Role::Assistant && !e.done {
                        e.done = true;
                        if let Some(u) = &usage {
                            e.usage = Some(TurnUsage {
                                input_tokens: u.input_tokens,
                                output_tokens: u.output_tokens,
                                cache_read: u.cache_read_tokens,
                                cache_write: u.cache_write_tokens,
                                reasoning: u.reasoning_tokens,
                                run_ms: 0,
                                llm_ms: 0,
                                ttft_ms: None,
                                route: String::new(),
                            });
                        }
                    }
                }
            }
            AgentEvent::TurnEnded { turn, .. } => {
                self.running = false;
                let elapsed = self.turn_started_at.map(|t| t.elapsed());
                // 冻结本轮统计快照（web turn tail：usage 药丸 + 用时对话框）
                let mut usage = std::mem::take(&mut self.turn_usage);
                if let (Some(total), Some(ttft_at)) = (elapsed, self.turn_first_token.take()) {
                    usage.run_ms = total.as_millis() as u64;
                    usage.llm_ms = total.saturating_sub(self.turn_tool_time).as_millis() as u64;
                    usage.ttft_ms = Some(ttft_at.saturating_duration_since(self.turn_started_at.unwrap_or(ttft_at)).as_millis() as u64);
                }
                usage.route = self.route_label.clone();
                let has_usage = usage.total_tokens() > 0;
                // 结转本轮 LLM/工具耗时（web sessionStats 的 llmMs/toolMs）
                if let Some(total) = elapsed {
                    self.session_llm_time += total.saturating_sub(self.turn_tool_time);
                    self.session_tool_time += self.turn_tool_time;
                }
                // 结转 TTFT 与纯解码窗口（web sessionStats 的 ttftMs/ttftSteps、
                // decodeMs/decodeTokens；rustdsh 无独立解码计时，以 llm − ttft
                // 近似——两侧都计到才进均值/速度）
                if let Some(ttft) = usage.ttft_ms {
                    self.session_ttft_sum_ms += ttft;
                    self.session_ttft_steps += 1;
                    if let Some(total) = elapsed {
                        let llm = total.saturating_sub(self.turn_tool_time).as_millis() as u64;
                        if llm > ttft {
                            self.session_decode_ms += llm - ttft;
                            self.session_decode_tokens += usage.output_tokens;
                        }
                    }
                }
                if let Some(e) = self.entries.last_mut() {
                    e.done = true;
                    e.elapsed = elapsed;
                    // 步结条目保留步级 usage（与回放同形）；轮总量只补无
                    // usage 的条目（异常轮无步结事件时的兜底）
                    if has_usage && e.usage.is_none() {
                        e.usage = Some(usage);
                    }
                    e.ended_at_ms = Some(now_ms());
                }
                self.turn_started_at = None;
                self.turn_open = None;
                // 该轮折尔回到 compact 默认（web：manual overrides 保留其余轮）
                self.turn_expanded.remove(&turn);
            }
            AgentEvent::Error { message, .. } => {
                self.running = false;
                self.turn_started_at = None;
                self.entries.push(ChatEntry {
                    role: Role::Error,
                    blocks: vec![MsgBlock::Text(message)],
                    done: true,
                    elapsed: None, usage: None, ended_at_ms: None,
                    turn: self.ui_turn,
                 context: None,
open: false,
});
            }
            AgentEvent::Compacted { .. } => {
                self.entries.push(ChatEntry { role: Role::Notice, blocks: vec![], done: true, elapsed: None, usage: None, ended_at_ms: None, turn: self.ui_turn, context: None,
open: false,
});
            }
        }
        // 智能吸底：只有用户本来就贴在底部时才跟随滚动（web 同款行为）；
        // 用户上翻阅读时不再被流式输出拽走。
        self.sync_chat_list(self.chat_near_bottom());
        app_level
    }

    /// 从会话日志回读指定工具调用的最新结果文本。
    fn latest_tool_result_text(&self, call_id: &str) -> (String, bool) {
        let session = self.agent.session();
        let session = session.lock().unwrap();
        for entry in session.entries().iter().rev() {
            if let SessionEvent::ToolResult { message, .. } = &entry.event
                && let Some(ContentBlock::ToolResult { tool_call_id, content, is_error }) =
                    message.content.first()
                && tool_call_id.0 == call_id
            {
                let text: String = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                return (text, is_error.unwrap_or(false));
            }
        }
        (String::new(), false)
    }

    /// 轮次过程折叠派生（web turn-process Definition 语义，1:1）：
    /// 已关闭轮 + 末条助手条目含非空文本且无工具块 → 该条目为答案边界；
    /// 边界前的助手条目（reasoning/早前回复/工具行）构成过程组。
    /// 错误行与 Notice（压缩检查点）独立在外，始终可见。
    /// 仅 compact 视图参与；返回 turn -> 折叠信息。
    fn turn_process_folds(&self) -> HashMap<u64, TurnFold> {
        let mut out = HashMap::new();
        if self.transcript_view != TranscriptView::Compact {
            return out;
        }
        let mut by_turn: HashMap<u64, Vec<usize>> = Default::default();
        for (i, e) in self.entries.iter().enumerate() {
            // 上下文注入也是过程证据（web：fold 进过程组，不计入摘要计数）
            if matches!(e.role, Role::Assistant | Role::Context) {
                by_turn.entry(e.turn).or_default().push(i);
            }
        }
        for (t, idxs) in by_turn {
            // 打开的轮次不折叠；答案自身不算过程（<2 条 = 无过程组）
            if self.turn_open == Some(t) || idxs.len() < 2 {
                continue;
            }
            let Some(answer) = idxs.iter().rev().find(|&&i| self.entries[i].role == Role::Assistant).copied() else {
                continue;
            };
            let answer_entry = &self.entries[answer];
            let has_text = answer_entry
                .blocks
                .iter()
                .any(|b| matches!(b, MsgBlock::Text(x) if !x.trim().is_empty()));
            let has_tool = answer_entry.blocks.iter().any(|b| matches!(b, MsgBlock::Tool(_)));
            if !has_text || has_tool {
                continue;
            }
            let process: Vec<usize> = idxs.iter().copied().take_while(|&i| i < answer).collect();
            if process.is_empty() {
                continue;
            }
            let (mut tools, mut subagents, mut messages) = (0usize, 0usize, 0usize);
            for &i in &process {
                let e = &self.entries[i];
                let mut reply = false;
                for b in &e.blocks {
                    match b {
                        // subagent 委派单独计数（web 同名规则：subagent / subagent_*）
                        MsgBlock::Tool(tool) => {
                            if tool.name == "subagent" || tool.name.starts_with("subagent_") {
                                subagents += 1;
                            } else {
                                tools += 1;
                            }
                        }
                        MsgBlock::Text(x) if !x.trim().is_empty() => reply = true,
                        _ => {}
                    }
                }
                if reply {
                    messages += 1;
                }
            }
            out.insert(t, TurnFold { first_process: process[0], answer, tools, messages, subagents });
        }
        out
    }

    // --- 渲染 ---------------------------------------------------------------

    fn block_element(&self, block: &MsgBlock, ei: usize, bi: usize, this: &Entity<Self>, content_w: f32) -> AnyElement {
        match block {
            MsgBlock::Attachment(_) => div().into_any_element(),
            MsgBlock::Text(t) => {
                // 流式尾喂活文本 + 50ms 稳态解析窗（vendor [dsh] 补丁：节流
                // 不因连续 delta 重置，约 20 次/秒渐进发布，对齐 web 每
                // 2-3 帧一次的流式节奏）；非尾块（已完结）直接用现文。
                let is_stream_tail = self.running
                    && ei + 1 == self.entries.len()
                    && bi + 1 == self.entries.get(ei).map(|e| e.blocks.len()).unwrap_or(0);
                div()
                    .w_full()
                    .text_color(theme::t().text)
                    .child(widgets::MarkdownBlock {
                        text: t.clone(),
                        id: 1_000_000 + ei * 1000 + bi,
                        parse_delay: is_stream_tail.then(|| Duration::from_millis(50)),
                    })
                    .into_any_element()
            }

            MsgBlock::Reasoning { text, open } => {
                let open = *open;
                let t = this.clone();
                // web ReasoningRow 语义：running = 本块是**正在流式消息的
                // 最后一个块**（streaming && i === last）。曾用 entry.done
                // 判定——done 只在整轮结束时打给最后一条，导致历史步骤的
                // Think 行整轮误扫。工具块一旦出现，本块即非尾，停止扫光。
                let active = self.running
                    && ei + 1 == self.entries.len()
                    && bi + 1 == self.entries.get(ei).map(|e| e.blocks.len()).unwrap_or(0);
                // web 结构：root(v_flex) > row(24px header) + thinkBody(展开体)
                // 展开体是 header 的兄弟节点，不在 24px 行内
                let mut header = div()
                    .id(("think-row", (ei * 1000 + bi) as u64))
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(MsgBlock::Reasoning { open: o, .. }) =
                                v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                            {
                                *o = !*o;
                            }
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(px(12.0))
                            .text_color(theme::t().text_2),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text)
                            .child("Think"),
                    )
                    .child(dot_sep())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            // web：运行时摘要跟随最新一行且尾部贴右（alpha.4
                            // data-follow-end 的 CSS 右贴 = 显示行尾、左侧
                            // 被裁），完成后回到首行
                            .child(if active {
                                let line = text.lines().last().unwrap_or("");
                                let chars: Vec<char> = line.chars().collect();
                                let tail: String = if chars.len() > 120 {
                                    chars[chars.len() - 120..].iter().collect()
                                } else {
                                    line.to_string()
                                };
                                tail
                            } else {
                                widgets::first_line(text)
                            }),
                    );
                if active {
                    // web .row::after 运行扫光（行容器 relative + overflow_hidden）；
                    // with_animation 每帧驱动 left，替代曾用的 100ms 全局 tick
                    header = header.child(
                        widgets::row_sweep_band().with_animation(
                            ("think-sweep", (ei * 1000 + bi) as u64),
                            Animation::new(Duration::from_millis(widgets::SWEEP_PERIOD_MS))
                                .repeat()
                                .with_easing(widgets::row_sweep_easing),
                            move |band, delta| {
                                band.left(px(-widgets::SWEEP_BAND + (content_w + widgets::SWEEP_BAND) * delta))
                            },
                        ),
                    );
                }
                let mut wrapper = div().w_full().v_flex().child(header);
                if open {
                    wrapper = wrapper.child(
                        div()
                            .pt_1()
                            .pb_1()
                            .pl(px(22.0))
                            .pr_2()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            // web .thinkBody：pre-wrap 语义——逐行渲染保留段落
                            .v_flex()
                            .gap(px(4.0))
                            .children(
                                text.lines().map(|l| {
                                    div().child(l.to_string())
                                })
                            ),
                    );
                }
                wrapper.into_any_element()
            }

            MsgBlock::Tool(tool) => {
                let open = tool.open;
                let (_, icon) = widgets::tool_display(&tool.name);
                let (title, summary, file_path) = widgets::tool_row_texts(&tool.name, &tool.arguments);
                // web ToolRow：失败行折叠摘要 = 输出首行（failureLine 替换语义）
                let failure = if tool.error && tool.result.is_some() {
                    Some(widgets::first_line(tool.result.as_deref().unwrap_or("")))
                } else {
                    None
                };
                let summary_text = failure.clone().unwrap_or_else(|| {
                    // 有链接的 read/write 摘要 = 相对化后的 path；
                    // list/exists 等无链接 op 的摘要同样剥工作区根/~ 前缀
                    // （web relativizeToCwd 作用于全部文件工具摘要）
                    let raw = file_path.as_deref().unwrap_or(summary.as_str());
                    widgets::display_path(raw, &self.cwd)
                });
                let t = this.clone();
                let running = tool.result.is_none();
                // Inspect 跳转目标：该调用在轨迹列表中的行号（之前的工具块计数）
                let mut traj_ix = 0usize;
                'traj_count: for (i, e) in self.entries.iter().enumerate() {
                    for (j, b) in e.blocks.iter().enumerate() {
                        if matches!(b, MsgBlock::Tool(_)) {
                            if i == ei && j == bi {
                                break 'traj_count;
                            }
                            traj_ix += 1;
                        }
                    }
                }
                // 新台账布局插入轮头条与非工具 cell：换算成滚动容器子节点下标
                let traj_ix = self.traj_child_index(traj_ix);
                let row_group: SharedString = format!("toolrow-{}-{}", ei, bi).into();
                let mut header = div()
                    .id(("tool-row", (ei * 1000 + bi) as u64))
                    .group(row_group.clone())
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(MsgBlock::Tool(tool)) =
                                v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                            {
                                // web 行为：点击工具行在下方原地展开/收起 IO 卡，
                                // 不强制打开右侧详情面板
                                tool.open = !tool.open;
                                let detail = ToolDetail {
                                    name: tool.name.clone(),
                                    arguments: tool.arguments.clone(),
                                    result: tool.result.clone(),
                                    error: tool.error,
                                };
                                v.app.update(cx, |a, cx| {
                                    a.selected_tool = Some(detail);
                                    cx.notify();
                                });
                            }
                            cx.notify();
                        });
                    })
                    .child({
                        // web DisclosureRow leading：16px 槽双层图标——静止为
                        // 工具图标（错误终态为状态点），行 hover 淡入向下
                        // 箭头；展开态常驻向下箭头
                        if open {
                            div()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::ChevronDown).size(px(14.0)).text_color(theme::t().text_3))
                        } else {
                            let idle: AnyElement = if tool.error && tool.result.is_some() {
                                // web ToolRow leadingFor：终态 error 用状态点替换工具图标
                                state_dot(theme::t().error).into_any_element()
                            } else {
                                Icon::new(icon).size(px(14.0)).text_color(theme::t().text_3).into_any_element()
                            };
                            div()
                                .relative()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_none()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(idle)
                                        .group_hover(row_group.clone(), |s| s.opacity(0.0)),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(Icon::new(IconName::ChevronDown).size(px(14.0)).text_color(theme::t().text_3))
                                        .opacity(0.0)
                                        .group_hover(row_group, |s| s.opacity(1.0)),
                                )
                        }
                    })
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text)
                            .child(title),
                    )
                    .child(dot_sep());
                // web fileLink：文件工具的 path 摘要渲染为下划线链接，
                // 点击用宿主默认应用打开（阻断行点击的展开切换）
                if file_path.is_some() && failure.is_none() {
                    let open_path = file_path.clone().unwrap_or_default();
                    header = header.child(
                        div()
                            .id(("tool-file", (ei * 1000 + bi) as u64))
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_2)
                            .underline()
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::t().text))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                widgets::open_with_host_app(&open_path);
                            })
                            .child(summary_text.clone()),
                    );
                } else {
                    header = header.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(if failure.is_some() { theme::t().error } else { theme::t().text_3 })
                            .child(summary_text.clone()),
                    );
                }
                if running {
                    // web .row::after 运行扫光：with_animation 每帧驱动 left
                    header = header.child(
                        widgets::row_sweep_band().with_animation(
                            ("tool-sweep", (ei * 1000 + bi) as u64),
                            Animation::new(Duration::from_millis(widgets::SWEEP_PERIOD_MS))
                                .repeat()
                                .with_easing(widgets::row_sweep_easing),
                            move |band, delta| {
                                band.left(px(-widgets::SWEEP_BAND + (content_w + widgets::SWEEP_BAND) * delta))
                            },
                        ),
                    );
                }
                // hover 显现 Inspect pill 的悬停域：标题行 + 展开体整体
                let group: SharedString = format!("tool-blk-{ei}-{bi}").into();
                let mut wrapper = div().w_full().v_flex().group(group.clone()).child(header);
                if open {
                    // web ToolRow 卡片分派：terminal/read/diff/web 各走专属
                    // 原语；错误行与未匹配工具回退通用 IO 卡（web 卡模型
                    // 在错误/缺元数据时为 null 的同一回退语义）
                    let args_json: serde_json::Value =
                        serde_json::from_str(&tool.arguments).unwrap_or(serde_json::Value::Null);
                    let arg_str = |key: &str| -> String {
                        args_json
                            .get(key)
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    };
                    let uid = (ei * 1000 + bi) as u64;
                    // web deriveBody：IO 卡的输入 = pretty JSON（解析失败回退原文）
                    let pretty_args = serde_json::from_str::<serde_json::Value>(&tool.arguments)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| tool.arguments.clone());
                    let element = match tool.name.as_str() {
                        "shell" => {
                            let t_fold = this.clone();
                            widgets::terminal_card(
                                uid,
                                &arg_str("command"),
                                &self.cwd,
                                tool.result.as_deref(),
                                running,
                                tool.error,
                                tool.expanded,
                                move |_, _, cx| {
                                    t_fold.update(cx, |v, cx| {
                                        if let Some(MsgBlock::Tool(tool)) =
                                            v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                        {
                                            tool.expanded = !tool.expanded;
                                        }
                                        // 折叠/展开改了条目高度：列表须重测
                                        v.invalidate_chat_heights_range(ei..ei + 1);
                                        cx.notify();
                                    });
                                },
                            )
                            .into_any_element()
                        }
                        "fs" if !tool.error && tool.result.is_some() => {
                            let path = arg_str("path");
                            let shown = widgets::display_path(&path, &self.cwd);
                            let result = tool.result.clone().unwrap_or_default();
                            match arg_str("op").as_str() {
                                "read" => {
                                    let lang = std::path::Path::new(&path)
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or("")
                                        .to_lowercase();
                                    let t_fold = this.clone();
                                    widgets::read_card(
                                        uid,
                                        &shown,
                                        &lang,
                                        &result,
                                        tool.expanded,
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .into_any_element()
                                }
                                "write" => {
                                    let content = arg_str("content");
                                    let t_fold = this.clone();
                                    widgets::diff_card(
                                        uid,
                                        &shown,
                                        &content,
                                        tool.expanded,
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .into_any_element()
                                }
                                _ => widgets::io_card(
                                    uid,
                                    &pretty_args,
                                    tool.result.as_deref(),
                                    tool.error,
                                    tool.expanded,
                                    {
                                        let t_fold = this.clone();
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        }
                                    },
                                )
                                .into_any_element(),
                            }
                        }
                        "grep" if !tool.error && tool.result.is_some() => {
                            match widgets::parse_grep_result(tool.result.as_deref().unwrap_or("")) {
                                Some(search) => {
                                    let t_fold = this.clone();
                                    let this_grp = this.clone();
                                    let mk_group = move |gi: usize| {
                                        let t = this_grp.clone();
                                        Box::new(move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                                            t.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    // 升序表内折叠/展开文件组
                                                    match tool.collapsed_groups.binary_search(&gi) {
                                                        Ok(pos) => {
                                                            tool.collapsed_groups.remove(pos);
                                                        }
                                                        Err(pos) => {
                                                            tool.collapsed_groups.insert(pos, gi);
                                                        }
                                                    }
                                                }
                                                cx.notify();
                                            });
                                        }) as Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App)>
                                    };
                                    widgets::search_card(
                                        uid,
                                        widgets::SearchCardData::Matches { search: &search },
                                        tool.expanded,
                                        &tool.collapsed_groups,
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        },
                                        Box::new(mk_group),
                                    )
                                    .into_any_element()
                                }
                                None => widgets::io_card(
                                    uid,
                                    &pretty_args,
                                    tool.result.as_deref(),
                                    tool.error,
                                    tool.expanded,
                                    {
                                        let t_fold = this.clone();
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        }
                                    },
                                )
                                .into_any_element(),
                            }
                        }
                        "glob" if !tool.error && tool.result.is_some() => {
                            match widgets::parse_glob_result(tool.result.as_deref().unwrap_or("")) {
                                Some(paths) => {
                                    let t_fold = this.clone();
                                    widgets::search_card(
                                        uid,
                                        widgets::SearchCardData::Paths { paths: &paths },
                                        tool.expanded,
                                        &tool.collapsed_groups,
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        },
                                        Box::new(|_| Box::new(|_, _, _| {})),
                                    )
                                    .into_any_element()
                                }
                                None => widgets::io_card(
                                    uid,
                                    &pretty_args,
                                    tool.result.as_deref(),
                                    tool.error,
                                    tool.expanded,
                                    {
                                        let t_fold = this.clone();
                                        move |_, _, cx| {
                                            t_fold.update(cx, |v, cx| {
                                                if let Some(MsgBlock::Tool(tool)) =
                                                    v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                                {
                                                    tool.expanded = !tool.expanded;
                                                }
                                                // 折叠/展开改了条目高度：列表须重测
                                                v.invalidate_chat_heights_range(ei..ei + 1);
                                                cx.notify();
                                            });
                                        }
                                    },
                                )
                                .into_any_element(),
                            }
                        }
                        "web_fetch" if !tool.error && tool.result.is_some() => widgets::web_fetch_card(
                            uid,
                            &arg_str("url"),
                            tool.result.as_deref().is_some_and(|r| r.chars().count() >= 8000),
                        )
                        .into_any_element(),
                        _ => widgets::io_card(
                            uid,
                            &pretty_args,
                            tool.result.as_deref(),
                            tool.error,
                            tool.expanded,
                            {
                                let t_fold = this.clone();
                                move |_, _, cx| {
                                    t_fold.update(cx, |v, cx| {
                                        if let Some(MsgBlock::Tool(tool)) =
                                            v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                                        {
                                            tool.expanded = !tool.expanded;
                                        }
                                        // 折叠/展开改了条目高度：列表须重测
                                        v.invalidate_chat_heights_range(ei..ei + 1);
                                        cx.notify();
                                    });
                                }
                            },
                        )
                        .into_any_element(),
                    };
                    wrapper = wrapper.child(element);
                    // web inspectButton：展开体下方左对齐小 pill，
                    // hover 整个工具块时显现，点击跳轨迹视图对应行
                    let t_insp = this.clone();
                    // gpui 无 align-self：外层全宽 flex 使 pill 靠左
                    wrapper = wrapper.child(
                        div().w_full().flex().child(
                            div()
                                .id(("tool-inspect", (ei * 1000 + bi) as u64))
                                .flex()
                                .items_center()
                                .gap_1()
                                .mt(px(4.0))
                                .mb(px(2.0))
                                .ml(px(4.0))
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded_full()
                                // web ToolRow .chip（alpha.4）：0.5px 发丝 + 色
                                // 阶升到 l4 补偿细线
                                .border(px(0.5))
                                .border_color(theme::t().border_l4)
                                .bg(theme::t().bg_base)
                                .text_color(theme::t().text_2)
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .cursor_pointer()
                                .opacity(0.0)
                                .group_hover(group.clone(), |s| s.opacity(1.0))
                                .hover(|s| s.bg(theme::t().elevated).text_color(theme::t().text))
                                .on_click(move |_, _, cx| {
                                    t_insp.update(cx, |v, cx| {
                                        v.tab = CenterTab::Trajectory;
                                        v.traj_ul.scroll_to_item(traj_ix, ScrollStrategy::Top);
                                        cx.notify();
                                    });
                                })
                                .child(Icon::new(IconName::Inspector).size(px(12.0)))
                                .child("查看"),
                        ),
                    );
                }
                wrapper.into_any_element()
            }
        }
    }

    fn render_entry(&self, entry: &ChatEntry, ei: usize, this: &Entity<Self>, content_w: f32) -> Div {
        match entry.role {
            Role::User => {
                let text = entry
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        MsgBlock::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let t = this.clone();
                let bubble_text = text.clone();
                let group: SharedString = format!("user-msg-{ei}").into();
                let group_copy = group.clone();
                // web MessageIconActions（alpha.4）：最新一条 user 行操作
                // 常显，更早的行 hover 才显现（:has(~ ) 后继兄弟选择器语义）
                let latest_user = self
                    .entries
                    .iter()
                    .rposition(|e| e.role == Role::User)
                    .is_some_and(|ix| ix == ei);
                let mut user_col = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(6.0))
                    .group(group);
                // 附件区：气泡外、气泡上方，右对齐可换行（web
                // .attachmentRow：flex-wrap、justify-end、gap8）；单图
                // 大图样式仅上游图片画廊有——混合行一律 64px tile
                let attach_count = entry
                    .blocks
                    .iter()
                    .filter(|b| matches!(b, MsgBlock::Attachment(_)))
                    .count();
                if attach_count > 0 {
                    let mut row = div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .max_w_full()
                        .gap(px(8.0));
                    for b in &entry.blocks {
                        if let MsgBlock::Attachment(attach) = b {
                            row = row.child(render_chat_attachment(
                                attach,
                                attach_count > 1,
                            ));
                        }
                    }
                    user_col = user_col.child(row);
                }
                user_col.child(
                        div()
                            .max_w(px(layout::user_bubble_max(content_w)))
                            .rounded(px(22.0))
                            // web --dsw-specific-bubble（亮=deepseek-50 淡蓝 /
                            // 暗=bluish-850）：不能用 surface——亮色下是纯
                            // 白，白底白泡等于没有展示效果。
                            .bg(theme::t().bubble)
                            .px_4()
                            .py(px(10.0))
                            .text_size(px(theme::FONT_BUBBLE))
                            .line_height(px(theme::FONT_BUBBLE_LEADING))
                            .text_color(theme::t().text)

                            .child(bubble_text),
                    )
                    .child(
                        // 气泡下方的复制按钮（web MessageIconActions：最新
                        // user 行常显，更早的行悬停显现）
                        div()
                            .id(("copy-user", ei as u64))
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(theme::t().caption)
                            .opacity(if latest_user { 1.0 } else { 0.0 })
                            .group_hover(group_copy, |s| s.opacity(1.0))
                            .hover(|s| s.text_color(theme::t().text_2).bg(theme::t().hover))
                            .tooltip(tip("复制"))
                            .on_click(move |_, _, cx| {
                                let text = text.clone();
                                t.update(cx, |_, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                });
                            })
                            .child(Icon::new(IconName::Copy).size(px(14.0))),
                    )
            }
            Role::Assistant => {
                let group: SharedString = format!("assistant-msg-{ei}").into();
                let mut col = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .group(group);
                for (bi, block) in entry.blocks.iter().enumerate() {
                    col = col.child(self.block_element(block, ei, bi, this, content_w));
                }
                if entry.done && let Some(elapsed) = entry.elapsed {
                    // web TurnTailNodeView：最新一轮 'always'，更早的轮 'hover'
                    let latest_turn = self.entries.last().is_some_and(|last| last.turn == entry.turn);
                    col = col.child(render_entry_footer(
                        elapsed,
                        entry.usage.clone(),
                        entry.ended_at_ms,
                        this,
                        ei,
                        latest_turn,
                    ));
                }
                div().w_full().child(col)
            }
            Role::Error => {
                let text = entry
                    .blocks
                    .first()
                    .map(|b| match b {
                        MsgBlock::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                div().w_full().child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .child(
                            div()
                                .mt(px(6.0))
                                .size(px(8.0))
                                .rounded_full()
                                .flex_none()
                                .bg(theme::t().error),
                        )
                        .child(
                            div().child(
                                div()
                                    .text_color(theme::t().error)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("出错了"),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().text_color(theme::t().text_2).child(text),
                        ),
                )
            }
            Role::Context => {
                // 上下文注入行（web ContextInjectionRow，DisclosureRow from
                // Figma 10:2482）：24px 头行（图标+标题+点+生产者+点+摘要）
                // + 展开体（代码底、141px 上限、11/16 mono tertiary）
                let info = entry.context.clone().unwrap_or_default();
                let open = entry.open;
                let t = this.clone();
                let body = entry
                    .blocks
                    .first()
                    .map(|b| match b {
                        MsgBlock::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let row_group: SharedString = format!("ctxrow-{}", ei).into();
                let mut header = div()
                    .id(("ctx-row", ei as u64))
                    .group(row_group.clone())
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(e) = v.entries.get_mut(ei) {
                                e.open = !e.open;
                            }
                            cx.notify();
                        });
                    })
                    .child({
                        // web DisclosureRow leading（同 tool row）：静止为
                        // 注入图标，行 hover 淡入向下箭头；展开态常驻箭头
                        if open {
                            div()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::ChevronDown).size(px(14.0)).text_color(theme::t().text_3))
                        } else {
                            div()
                                .relative()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_none()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            gpui::svg()
                                                .path("icons/context-injection.svg")
                                                .w(px(14.0))
                                                .h(px(14.0))
                                                .flex_none()
                                                .text_color(theme::t().text_3),
                                        )
                                        .group_hover(row_group.clone(), |s| s.opacity(0.0)),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(Icon::new(IconName::ChevronDown).size(px(14.0)).text_color(theme::t().text_3))
                                        .opacity(0.0)
                                        .group_hover(row_group, |s| s.opacity(1.0)),
                                )
                        }
                    })
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text)
                            .child(info.title),
                    );
                if let Some(label) = &info.label {
                    header = header.child(dot_sep()).child(
                        div()
                            .flex_none()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            .child(label.clone()),
                    );
                }
                if let Some(summary) = &info.summary {
                    header = header.child(dot_sep()).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            .child(summary.clone()),
                    );
                }
                let mut wrap = div().w_full().v_flex().child(header);
                if open {
                    wrap = wrap.child(
                        div()
                            .id(("ctx-body", ei as u64))
                            .ml(px(22.0))
                            .mt(px(4.0))
                            .max_h(px(141.0))
                            .overflow_y_scroll()
                            .rounded(px(8.0))
                            .bg(theme::t().code_bg)
                            .pt(px(10.0))
                            .pr(px(16.0))
                            .pb(px(12.0))
                            .pl(px(12.0))
                            .font_family(widgets::theme_mono())
                            .text_size(px(11.0))
                            .line_height(px(16.0))
                            .text_color(theme::t().text_3)
                            .child(body),
                    );
                }
                div().w_full().v_flex().child(wrap)
            }
            Role::Notice => {
                // 压缩分隔条：居中 hairline + 说明文字（web compaction 提示行；
                // alpha.4 全局 hairline 化，线宽随 0.5px）
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(4.0))
                    .child(div().flex_1().h(px(0.5)).bg(theme::t().border_l2))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(theme::FONT_CAPTION))
                            .line_height(px(theme::FONT_CAPTION_LEADING))
                            .text_color(theme::t().text_3)
                            .child("上下文已压缩 · 已生成摘要检查点"),
                    )
                    .child(div().flex_1().h(px(0.5)).bg(theme::t().border_l2))
            }
        }
    }

    /// 消息流底部进行中的状态行（web ChatView .turnStatus）。
    fn render_status_line(&self) -> Div {
        div().w_full().child(
            div()
                .h(px(26.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::t().accent)
                        .child("Deep diving…"),
                )
                .children(self.turn_started_at.map(|t| {
                    div()
                        .ml_2()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(theme::t().caption)
                        .child(format!("{}秒", t.elapsed().as_secs()))
                })),
        )
    }

    /// 轨迹 tab：轮次分组的事件台账（上游 ui-trajectory TrajectoryTable 的
    /// GPUI 对应物）。行 = 38px 卡片 cell（序号 + 种类 tag 药丸 + 摘要 +
    /// 右 320px 指标栏）；每轮一条 44px 吸顶轮头条（列标签 输入/输出/思考/时间）。
    /// 滚动走 uniform_list 虚拟化（上游 web 同有 virtualSpacer）：只渲染视口
    /// 内 item，整表重排的帧成本消失；Inspect pill 经 scroll_to_item 跳转。
    fn render_trajectory(&self, this: &Entity<Self>, width: f32, _cx: &App) -> Stateful<Div> {
        // --- 平坦行模型 + 累积顶边（item 高度由 mb 几何固定：52/48/60）---
        let (rows_model, tops) = self.traj_rows();
        let cells_rc: Rc<Vec<TrajCell>> = Rc::new(self.traj_cells());
        let rows_rc: Rc<Vec<TrajRow>> = Rc::new(rows_model);
        let t = theme::t();
        let lane = TRAJ_LANE_MAX_PX.min(width);
        // --- 吸顶轮头覆盖层（CSS position:sticky 的 GPUI 等价：固定行高可算
        // 出各段偏移；轮头过视口顶即贴顶，段尾临近随段尾上移、不越出父段）---
        let scrolled = -self.traj_ul.offset().y.to_f64() as f32;
        // 轮头位置表：(轮号, 顶边)；段尾 = 下一轮头顶边，末段 = 表尾总高
        let headers: Vec<(u64, f32)> = rows_rc
            .iter()
            .zip(tops.iter())
            .filter_map(|(r, &top)| match r {
                TrajRow::Header(turn) => Some((*turn, top)),
                _ => None,
            })
            .collect();
        let total_h = match (rows_rc.last(), tops.last()) {
            (Some(TrajRow::Header(_)), Some(&top)) => top + TRAJ_HEADER_PX + TRAJ_BODY_PAD_T_PX,
            (Some(TrajRow::Cell(_)), Some(&top)) => top + TRAJ_CELL_PX + TRAJ_BODY_PAD_B_PX,
            _ => 0.0,
        };
        let mut sticky: Option<(u64, f32)> = None;
        for (k, &(turn, header_top)) in headers.iter().enumerate() {
            let section_bottom = headers.get(k + 1).map(|h| h.1).unwrap_or(total_h);
            // CSS sticky top:0：轮头过视口顶才画覆盖层；贴顶后恒 0，段尾
            // 临近随段尾上移、不越出父段。
            if scrolled > header_top && scrolled < section_bottom {
                let top = (header_top - scrolled)
                    .max(0.0)
                    .min(section_bottom - TRAJ_HEADER_PX - scrolled);
                sticky = Some((turn, top));
            }
        }
        // --- 虚拟化列表：item 渲染只取视口 range ---
        let ul_this = this.clone();
        let ul_cells = Rc::clone(&cells_rc);
        let ul_rows = Rc::clone(&rows_rc);
        let list = gpui::uniform_list(
            "traj-list",
            rows_rc.len(),
            move |range, _window, cx| {
                let (selected, selected_msg) = ul_this.read_with(cx, |v, cx2| {
                    v.app.read_with(cx2, |a, _| {
                        (
                            a.selected_tool.clone(),
                            a.selected_message.as_ref().map(|m| m.index),
                        )
                    })
                });
                range
                    .map(|ix| match ul_rows[ix] {
                        TrajRow::Header(turn) => ul_this
                            .read_with(cx, |v, _| v.traj_turn_header(turn, 0.0))
                            // uniform_list item 不继承交叉轴 stretch：通栏
                            // 铺底必须显式 w_full（否则 fit-content 缩左半）
                            .bar()
                            .w_full()
                            .mb(px(TRAJ_BODY_PAD_T_PX))
                            .into_any_element(),
                        TrajRow::Cell(ci) => {
                            let last_in_turn =
                                matches!(ul_rows.get(ix + 1), Some(TrajRow::Header(_)) | None);
                            let row = ul_this.read_with(cx, |v, _| {
                                v.traj_cell_row(
                                    &ul_cells[ci],
                                    selected.as_ref(),
                                    &ul_this,
                                    0,
                                    lane,
                                    selected_msg,
                                )
                            });
                            div()
                                .w_full()
                                .flex()
                                .justify_center()
                                .child(
                                    row.mb(px(if last_in_turn {
                                        TRAJ_BODY_PAD_B_PX
                                    } else {
                                        TRAJ_CELL_GAP_PX
                                    })),
                                )
                                .into_any_element()
                        }
                    })
                    .collect()
            },
        )
        .track_scroll(self.traj_ul.clone())
        .w_full()
        .h_full();
        // 轮头通栏铺底 + 内层 880 居中道；行卡 848（道内 padding 16）居中——
        // 行右 320px 指标栏与轮头列标签同靠居中道右缘（lane-24）对齐。
        let mut col = div()
            .id("traj-col")
            .relative()
            .h_full()
            .w_full()
            .child(list);
        if cells_rc.is_empty() {
            col = col.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(t.text_3)
                    .child("本轮还没有工具调用记录"),
            );
        } else if let Some((turn, top)) = sticky {
            col = col.child(
                div()
                    .absolute()
                    .top(px(top))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(self.traj_turn_header(turn, width).sticky_overlay(lane)),
            );
        }
        col
    }

    /// 台账 cell 派生：user/context/notice → 用户 cell；assistant → 消息 cell
    /// （带 usage 三指标与 llm 用时）+ 逐工具 cell。序号全局连续（上游 #index）。
    /// 台账平坦行模型（虚拟化与吸顶算式同源）：轮头 item 52px（44 条 +
    /// 8 进轮体）、cell item 48px（38 + 10 gap）、轮尾 cell 60px（38 + 22）。
    fn traj_rows(&self) -> (Vec<TrajRow>, Vec<f32>) {
        let cells = self.traj_cells();
        let mut rows: Vec<TrajRow> = Vec::new();
        let mut tops: Vec<f32> = Vec::new();
        let mut y = 0.0f32;
        let mut last_turn: Option<u64> = None;
        for (ci, c) in cells.iter().enumerate() {
            if last_turn != Some(c.turn) {
                rows.push(TrajRow::Header(c.turn));
                tops.push(y);
                y += TRAJ_HEADER_PX + TRAJ_BODY_PAD_T_PX;
                last_turn = Some(c.turn);
            }
            rows.push(TrajRow::Cell(ci));
            tops.push(y);
            let last_in_turn = cells.get(ci + 1).map(|n| n.turn != c.turn).unwrap_or(true);
            y += TRAJ_CELL_PX
                + if last_in_turn {
                    TRAJ_BODY_PAD_B_PX
                } else {
                    TRAJ_CELL_GAP_PX
                };
        }
        (rows, tops)
    }

    fn traj_cells(&self) -> Vec<TrajCell> {
        let mut cells: Vec<TrajCell> = Vec::new();
        for entry in &self.entries {
            let texts: Vec<&String> = entry
                .blocks
                .iter()
                .filter_map(|b| match b {
                    MsgBlock::Text(s) => Some(s),
                    _ => None,
                })
                .collect();
            // 单行 cell：内部换行压空格（nowrap 下 \n 仍会断行撑破 38px 行高）
            let joined = texts
                .iter()
                .map(|s| s.as_str().replace('\n', " "))
                .collect::<Vec<_>>()
                .join(" ");
            // 详情面板原文（保留换行）
            let raw = texts
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            match entry.role {
                Role::User | Role::Context | Role::Notice => {
                    if joined.trim().is_empty() {
                        continue;
                    }
                    cells.push(TrajCell {
                        turn: entry.turn,
                        kind: TrajKind::User,
                        index: cells.len() + 1,
                        text: joined,
                        dim: false,
                        detail_text: raw,
                        reasoning: None,
                        metrics: None,
                        time_ms: None,
                        tool: None,
                    });
                }
                Role::Assistant | Role::Error => {
                    // 上游消息行摘要回退链（layout.ts）：text → reasoning 预览
                    // → 有工具调用时「仅工具调用」（layout.toolCallOnly）；
                    // 缺这条链时纯工具调用步在台账里是空白行。
                    let reasoning = entry
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            MsgBlock::Reasoning { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .map(|s| s.replace('\n', " "))
                        .collect::<Vec<_>>()
                        .join(" ");
                    // 详情「思考」节原文（保留换行）
                    let reasoning_raw = entry
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            MsgBlock::Reasoning { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let reasoning_opt = if reasoning_raw.trim().is_empty() {
                        None
                    } else {
                        Some(reasoning_raw)
                    };
                    let has_tool = entry.blocks.iter().any(|b| matches!(b, MsgBlock::Tool(_)));
                    let (text, dim) = if !joined.trim().is_empty() {
                        (joined, false)
                    } else if !reasoning.trim().is_empty() {
                        (reasoning, false)
                    } else if has_tool {
                        ("仅工具调用".to_string(), true)
                    } else {
                        (joined, false)
                    };
                    cells.push(TrajCell {
                        turn: entry.turn,
                        kind: TrajKind::Message,
                        index: cells.len() + 1,
                        text,
                        dim,
                        detail_text: raw,
                        reasoning: reasoning_opt,
                        metrics: entry
                            .usage
                            .as_ref()
                            .map(|u| (u.input_tokens, u.output_tokens, u.reasoning)),
                        time_ms: entry.usage.as_ref().map(|u| u.llm_ms).filter(|v| *v > 0),
                        tool: None,
                    });
                    for block in &entry.blocks {
                        if let MsgBlock::Tool(tool) = block {
                            let (_, summary, _) =
                                widgets::tool_row_texts(&tool.name, &tool.arguments);
                            cells.push(TrajCell {
                                turn: entry.turn,
                                kind: TrajKind::Tool,
                                index: cells.len() + 1,
                                text: widgets::display_path(&summary, &self.cwd),
                                dim: false,
                                detail_text: String::new(),
                                reasoning: None,
                                metrics: None,
                                time_ms: None,
                                tool: Some(tool.clone()),
                            });
                        }
                    }
                }
            }
        }
        cells
    }

    /// Inspect 跳转行号换算：第 ord 个工具 cell（0 起，旧工具块计数序）在
    /// 新台账滚动容器里的子节点下标（轮头条与非工具 cell 都占位）。
    fn traj_child_index(&self, ord: usize) -> usize {
        let cells = self.traj_cells();
        let mut p = 0usize;
        let mut seen = 0usize;
        for (i, c) in cells.iter().enumerate() {
            if c.kind == TrajKind::Tool {
                if seen == ord {
                    p = i;
                    break;
                }
                seen += 1;
            }
        }
        let mut headers = 0usize;
        let mut last: Option<u64> = None;
        for c in cells.iter().take(p + 1) {
            if last != Some(c.turn) {
                headers += 1;
                last = Some(c.turn);
            }
        }
        p + headers
    }

    /// 44px 轮头条（上游 TrajectoryTurnHeader）：轮次标题 + 四列标签。
    fn traj_turn_header(&self, turn: u64, width: f32) -> TrajHeader {
        let _ = width;
        TrajHeader { turn }
    }

    /// 38px cell 行（上游 TrajectoryCell）：#序号 + tag 药丸 + 摘要 + 指标栏。
    fn traj_cell_row(
        &self,
        c: &TrajCell,
        selected: Option<&ToolDetail>,
        this: &Entity<Self>,
        _section: usize,
        lane: f32,
        selected_msg: Option<usize>,
    ) -> Stateful<Div> {
        let t = theme::t();
        let (tag_label, tag_fg, tag_bg) = match c.kind {
            TrajKind::User => ("用户", t.green, t.success_tertiary),
            TrajKind::Message => ("消息", t.tag_message_fg, t.tag_message_bg),
            TrajKind::Tool => ("工具", t.warn_label, t.warn_tertiary),
        };
        let is_sel = match (&c.tool, selected) {
            (Some(tool), Some(sel)) => sel.name == tool.name && sel.arguments == tool.arguments,
            _ => false,
        } || selected_msg == Some(c.index);
        // 选中环：上游 .selected 是 inset 2px 品牌环（不改布局）；GPUI 无
        // inset shadow，用 absolute inset_0 的 2px 环覆盖层等价——行边框恒
        // 0.5px，选中不产生 0.5→2px 的内容内缩跳动。
        let mut row = div()
            .id(("traj", c.index as u64))
            .relative()
            .flex()
            // 滚动列是定高 column 容器：子项默认 flex_shrink=1 会把 38px 行
            // 按溢出比例压扁（实测压到 14px），固定行高必须 flex_none。
            .flex_none()
            .items_center()
            .border(px(0.5))
            .border_color(t.border_l4)
            .rounded(px(8.0))
            .bg(t.layer3)
            .h(px(TRAJ_CELL_PX))
            // 行卡 = 880 道内缩 16×2（上游 .body padding 0 16）；居中由调用
            // 方 w_full + justify_center 外壳承担（item 内 margin 不可靠）
            .w(px((lane - 32.0).max(0.0)))
            .overflow_hidden()
            .pl(px(20.0))
            .pr(px(8.0))
            .gap(px(24.0));
        if c.kind == TrajKind::Tool {
            let tool = c.tool.clone().unwrap();
            let th = this.clone();
            let name = tool.name.clone();
            let arguments = tool.arguments.clone();
            let result = tool.result.clone();
            let error = tool.error;
            row = row
                .cursor_pointer()
                .hover(|s| s.bg(theme::t().hover))
                .on_click(move |_, _, cx| {
                    th.update(cx, |v, cx| {
                        let detail = ToolDetail {
                            name: name.clone(),
                            arguments: arguments.clone(),
                            result: result.clone(),
                            error,
                        };
                        v.app.update(cx, |a, cx| {
                            a.selected_tool = Some(detail);
                            a.selected_message = None;
                            a.details_open = true;
                            cx.notify();
                        });
                    });
                });
        } else {
            // 消息/用户 cell 点击开详情（上游 message record 详情面）；
            // 与工具选中互斥
            let detail = MessageDetail {
                index: c.index,
                kind_label: match c.kind {
                    TrajKind::User => "用户",
                    _ => "消息",
                },
                turn: c.turn,
                text: c.detail_text.clone(),
                reasoning: c.reasoning.clone(),
                usage: c.metrics,
            };
            let th = this.clone();
            row = row
                .cursor_pointer()
                .hover(|s| s.bg(theme::t().hover))
                .on_click(move |_, _, cx| {
                    th.update(cx, |v, cx| {
                        v.app.update(cx, |a, cx| {
                            a.selected_message = Some(detail.clone());
                            a.selected_tool = None;
                            a.details_open = true;
                            cx.notify();
                        });
                    });
                });
        }
        row = row
            // #序号（24px 三级色）
            .child(
                div()
                    .flex_none()
                    .w(px(24.0))
                    .whitespace_nowrap()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(t.text_3)
                    .child(format!("#{}", c.index)),
            )
            // 种类 tag 药丸（80px 槽 / 22px 丸 / 6px 圆角）
            .child(
                div()
                    .flex_none()
                    .w(px(80.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(px(22.0))
                            .px_1()
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .bg(tag_bg)
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tag_fg)
                            .child(tag_label),
                    ),
            )
            // 摘要（主文字色 + 单行省略；错误终态前置状态点）
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .overflow_hidden()
                    .when(
                        c.tool.as_ref().is_some_and(|tl| tl.error && tl.result.is_some()),
                        |s| s.child(state_dot(t.error)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            // 上游工具行内容等宽 12px（[data-kind='tool']
                            // .contentText），消息/用户行比例 13px
                            .when(c.kind == TrajKind::Tool, |s| {
                                s.font_family(widgets::theme_mono()).text_size(px(12.0))
                            })
                            .text_color(if c.dim { t.text_3 } else { t.text })
                            .child(c.text.clone()),
                    ),
            )
            // 右 320px 指标栏（4×71 + 3×12；消息行三指标，其余占位空串）
            .child(
                div()
                    .flex_none()
                    .w(px(320.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(12.0))
                    .child(traj_metric(
                        c.metrics.as_ref().map(|m| m.0.to_string()).unwrap_or_default(),
                    ))
                    .child(traj_metric(
                        c.metrics.as_ref().map(|m| m.1.to_string()).unwrap_or_default(),
                    ))
                    .child(traj_metric(
                        c.metrics
                            .as_ref()
                            .and_then(|m| m.2)
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    ))
                    .child(traj_metric(match c.time_ms {
                        Some(ms) => format!(
                            "{} 毫秒",
                            ms.to_string()
                                .as_bytes()
                                .rchunks(3)
                                .rev()
                                .map(|c| std::str::from_utf8(c).unwrap_or_default())
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                        None => "—".to_string(),
                    })),
            );
        // 选中环覆盖层最后渲染（画在行内容之上）；无 click handler，点击
        // 冒泡到行自身的 on_click。
        if is_sel {
            row = row.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(8.0))
                    .border(px(2.0))
                    .border_color(t.accent),
            );
        }
        row
    }
}

/// 台账行高规格（上游 TrajectoryCell/TrajectoryTurnHeader module.css 实值）。
const TRAJ_CELL_PX: f32 = 38.0;
const TRAJ_HEADER_PX: f32 = 44.0;
/// 轮头条内容道最大宽（上游 .inner max-width: 880px）。
const TRAJ_LANE_MAX_PX: f32 = 880.0;
/// 轮体规格（上游 TrajectoryTurn .body：gap 10、padding 8/16/22）。
const TRAJ_CELL_GAP_PX: f32 = 10.0;
const TRAJ_BODY_PAD_T_PX: f32 = 8.0;
const TRAJ_BODY_PAD_B_PX: f32 = 22.0;

/// 轨迹台账 cell（上游 TrajectoryCellProps 的 rustdsh 子集）。
struct TrajCell {
    turn: u64,
    kind: TrajKind,
    /// 全局序号（上游 #index，1 起）。
    index: usize,
    text: String,
    /// 回退摘要行（「仅工具调用」等），三级色渲染（上游 .toolCallOnly）。
    dim: bool,
    /// 详情面板用原文（保留换行；text 是单行省略版）。
    detail_text: String,
    /// 思考块拼接（消息 cell 详情「思考」节）。
    reasoning: Option<String>,
    /// 消息行 usage 三指标（输入/输出/思考）。
    metrics: Option<(u64, u64, Option<u64>)>,
    /// 时长列（消息行 = 轮 llm 用时；rustdsh 日志无事件级时间戳，工具行恒 —）。
    time_ms: Option<u64>,
    tool: Option<ToolBlock>,
}

#[derive(Clone, Copy, PartialEq)]
enum TrajKind {
    User,
    Message,
    Tool,
}

/// 台账平坦行（uniform_list 虚拟化 item）：轮头条或 cell（cells 下标）。
#[derive(Clone, Copy)]
enum TrajRow {
    Header(u64),
    Cell(usize),
}

/// 指标/时间列单元（71px 定宽三级色，上游 .metric/.time）。
fn traj_metric(text: String) -> Div {
    div()
        .flex_none()
        .w(px(71.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(13.0))
        .line_height(px(20.0))
        .text_color(theme::t().text_3)
        .child(text)
}

/// 轮头条构建器（流内与吸顶覆盖层共用同一规格）。
struct TrajHeader {
    turn: u64,
}

impl TrajHeader {
    /// 通栏铺底（上游 .root width:100%），内层 880 居中道 px16（.inner）：
    /// 列标签 mr8 使右缘 = lane-24，与行卡（lane-16 右缘、pr8）同线。
    fn bar(&self) -> Div {
        let t = theme::t();
        div()
            .flex_none()
            .h(px(TRAJ_HEADER_PX))
            .bg(t.ghost_active)
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex_grow()
                    .max_w(px(TRAJ_LANE_MAX_PX))
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(format!("第 {} 轮", self.turn)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(320.0))
                            .mr_2()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(traj_col_label("输入"))
                            .child(traj_col_label("输出"))
                            .child(traj_col_label("思考"))
                            .child(traj_col_label("时间")),
                    ),
            )
    }

    /// 吸顶覆盖层包装：通栏宽（外层 absolute 左右锚定），内层道自动居中。
    fn sticky_overlay(&self, lane: f32) -> Div {
        let _ = lane;
        self.bar().w_full()
    }
}

/// 轮头条列标签（上游 .column：71px 定宽二级色）。
fn traj_col_label(text: &'static str) -> Div {
    div()
        .flex_none()
        .w(px(71.0))
        .text_size(px(13.0))
        .line_height(px(20.0))
        .text_color(theme::t().text_2)
        .child(text)
}

// --- 轮次导航栏（web TurnNavigator，上游 0.1.2-alpha.3） ---------------------

/// 固定行距：相邻标记间距（web TURN_SPACING_PX）；溢出在框架内滚动。
const TURN_SPACING_PX: f32 = 10.0;
/// 梯顶/梯底各留白（web RAIL_INSET_PX）。
const RAIL_INSET_PX: f32 = 6.0;
/// 可滚动端留出的渐隐带（web FADE_PX）。
const RAIL_FADE_PX: f32 = 24.0;
/// 导航栏框架宽度（web .frame width: 28px）。
const RAIL_WIDTH_PX: f32 = 28.0;
/// 框架最大高（web height: min(…, 420px)；band-64 由外层 py 近似）。
const RAIL_MAX_HEIGHT_PX: f32 = 420.0;
/// 预览卡宽（web .preview width: min(300px, 100cqw-120px)）。
const RAIL_PREVIEW_WIDTH_PX: f32 = 300.0;
/// 预览卡最大高（web --turn-preview-height: 100px）。
const RAIL_PREVIEW_HEIGHT_PX: f32 = 100.0;
/// 中栏窄于此宽整条隐藏（web @container max-width: 900px）。
const RAIL_MIN_COLUMN_PX: f32 = 900.0;

/// 一条导航梯标记（web TurnRailItem）。已加载轮点击滚动到对应行；
/// rustdsh 全量加载会话，outline-only 标记只出现在「无可见条目」的轮次
/// （web 的 unloaded load-and-jump 在此退化为不可点，标记照常可 hover）。
#[derive(Clone)]
struct TurnRailItem {
    turn: u64,
    /// 有界 prompt 预览（已加载窗口优先，outline 补空）。
    prompt: String,
    /// 有界 response 预览（已加载窗口优先，outline 补空）。
    response: String,
    /// 已加载锚（该轮用户条目，缺席时取首条可见条目）；None = outline-only。
    anchor_ix: Option<usize>,
}

impl ChatView {
    /// 合并宿主 `turnOutline` 投影与已加载条目为完整导航梯（web
    /// mergeTurnRailItems：已加载轮保留其锚，outline 只补空预览与缺席轮；
    /// 结果按轮次号升序）。
    fn rail_items(&self) -> Vec<TurnRailItem> {
        struct Loaded {
            first_ix: Option<usize>,
            user_ix: Option<usize>,
            prompt: String,
            response: String,
        }
        impl Loaded {
            fn new() -> Self {
                Self { first_ix: None, user_ix: None, prompt: String::new(), response: String::new() }
            }
        }
        // 条目按轮时序连续（轮次号单调），按 run 分组即每轮一组。
        let mut turns: Vec<(u64, Loaded)> = Vec::new();
        for (ix, e) in self.entries.iter().enumerate() {
            if turns.last().map(|(t, _)| *t) != Some(e.turn) {
                turns.push((e.turn, Loaded::new()));
            }
            let loaded = &mut turns.last_mut().unwrap().1;
            if loaded.first_ix.is_none() {
                loaded.first_ix = Some(ix);
            }
            match e.role {
                // 首条用户条目：锚 + prompt 预览（同轮更晚的人类消息不改）
                Role::User if loaded.user_ix.is_none() => {
                    loaded.user_ix = Some(ix);
                    loaded.prompt = preview_parts(
                        e.blocks.iter().filter_map(|b| match b {
                            MsgBlock::Text(t) => Some(t.as_str()),
                            _ => None,
                        }),
                        PROMPT_PREVIEW_LIMIT,
                    );
                }
                // 最新带文本的助手条目胜出（web findLast non-empty）
                Role::Assistant => {
                    let parts: Vec<&str> = e
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            MsgBlock::Text(t) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        loaded.response = preview_parts(parts, RESPONSE_PREVIEW_LIMIT);
                    }
                }
                _ => {}
            }
        }
        let mut merged: Vec<TurnRailItem> = turns
            .into_iter()
            .map(|(turn, l)| TurnRailItem {
                turn,
                prompt: l.prompt,
                response: l.response,
                anchor_ix: l.user_ix.or(l.first_ix),
            })
            .collect();
        // outline：投影缺席 = 无该单元（仅已加载侧成梯）；在则补空预览与
        // 缺席轮。预览预算两侧一致（50/120），轮次加载前后显示同样的文字。
        for entry in self.rail_outline() {
            match merged.iter_mut().find(|i| i.turn == entry.turn) {
                Some(item) => {
                    if item.prompt.is_empty() {
                        item.prompt = entry.prompt;
                    }
                    if item.response.is_empty() {
                        item.response = entry.response;
                    }
                }
                None => merged.push(TurnRailItem {
                    turn: entry.turn,
                    prompt: entry.prompt,
                    response: entry.response,
                    anchor_ix: None,
                }),
            }
        }
        merged.sort_by_key(|i| i.turn);
        merged
    }

    /// 宿主 `turnOutline` wire 视图（身份门控缓存：轮内 draft-only 变化
    /// 返回同一个 Arc，这里的反序列化结果随之稳定）。
    fn rail_outline(&self) -> Vec<dsh_session_projection::turn_outline::TurnOutlineEntry> {
        let session = self.agent.session();
        let session = session.lock().unwrap();
        self.agent
            .projections()
            .view_of(&session, "turnOutline")
            .and_then(|v| serde_json::from_value((*v).clone()).ok())
            .unwrap_or_default()
    }

    /// 渲染导航栏：右缘竖直居中的 28px 框架内放固定行距标记梯；梯溢出
    /// 框架时在框内滚动（无滚动条，可滚端 24px 渐隐）；hover 出预览卡；
    /// 点击已加载标记滚动到该轮。条目 <2 或中栏 <900px 时整条缺席。
    fn render_turn_rail(
        &mut self,
        this: &Entity<Self>,
        items: &[TurnRailItem],
        column_w: f32,
    ) -> Option<AnyElement> {
        if items.len() < 2 || column_w < RAIL_MIN_COLUMN_PX {
            return None;
        }
        let natural_h = (items.len() - 1) as f32 * TURN_SPACING_PX + 2.0 * RAIL_INSET_PX;
        let frame_h = natural_h.min(RAIL_MAX_HEIGHT_PX);
        let scroll_top = f32::from(self.rail_scroll.0.borrow().base_handle.offset().y);

        // active 跟随：激活标记离开滚动视口（渐隐带算离屏）时居中；指针
        // 在栏内工作时暂停（web pointerInside 同语义）。
        let active_ix = self.active_turn.and_then(|t| items.iter().position(|i| i.turn == t));
        if let (Some(a_ix), Some(a_turn)) = (active_ix, self.active_turn) {
            if !self.rail_pointer_inside {
                let mark_mid = a_ix as f32 * TURN_SPACING_PX + RAIL_INSET_PX;
                let offscreen = mark_mid < scroll_top + RAIL_FADE_PX
                    || mark_mid > scroll_top + frame_h - RAIL_FADE_PX;
                if offscreen {
                    if self.rail_follow.get() != Some(a_turn) {
                        self.rail_follow.set(Some(a_turn));
                        self.rail_scroll.scroll_to_item(a_ix, ScrollStrategy::Center);
                    }
                } else {
                    self.rail_follow.set(None);
                }
            }
        }

        let items_rc: Rc<Vec<TurnRailItem>> = Rc::new(items.to_vec());
        let row_this = this.downgrade();
        let list_items = Rc::clone(&items_rc);
        let marks = gpui::uniform_list("turn-rail-marks", items.len(), move |range, _window, cx| {
            let (active_turn, preview_turn) = row_this
                .read_with(cx, |v, _| (v.active_turn, v.preview_turn))
                .unwrap_or((None, None));
            range
                .map(|ix| {
                    let item = &list_items[ix];
                    let active = Some(item.turn) == active_turn;
                    let previewing = Some(item.turn) == preview_turn;
                    // tick 状态序：active 20px 蓝 > hover 18px 灰 > 常态
                    // 12px 边框色（web .markActive/.markPreview/::before）。
                    let (tick_w, tick_color): (f32, Hsla) = if active {
                        (20.0, theme::t().accent.into())
                    } else if previewing {
                        (18.0, theme::t().text_3.into())
                    } else {
                        (12.0, theme::t().border_l4)
                    };
                    let t = row_this.clone();
                    let t_click = row_this.clone();
                    let turn = item.turn;
                    let anchor_ix = item.anchor_ix;
                    div()
                        .id(("rail-mark", ix as u64))
                        .h(px(TURN_SPACING_PX))
                        .w_full()
                        .relative()
                        .when(item.anchor_ix.is_none(), |d| d.opacity(0.6))
                        .cursor_pointer()
                        .on_hover(move |hovered, _, cx| {
                            let _ = t.update(cx, |v, cx| {
                                let next = hovered.then_some(turn);
                                if v.preview_turn != next {
                                    v.preview_turn = next;
                                    cx.notify();
                                }
                            });
                        })
                        .on_click(move |_, _, cx| {
                            let Some(ix) = anchor_ix else { return };
                            let _ = t_click.update(cx, |v, cx| {
                                // 跳入历史即离开贴底：先交出贴底所有权，
                                // 否则钉底跟随会把跳转拽回尾部（web 点击
                                // 释放 bottom ownership）
                                v.list_bottom.set(false);
                                // 行顶落在视口顶下 24px（web flowTop - 24）
                                v.chat_list
                                    .scroll_to(ListOffset { item_ix: ix, offset_in_item: px(-24.0) });
                                v.active_turn = Some(turn);
                                cx.notify();
                            });
                        })
                        .child(
                            div()
                                .absolute()
                                .right_0()
                                .top(px((TURN_SPACING_PX - 2.0) / 2.0))
                                .h(px(2.0))
                                .w(px(tick_w))
                                .rounded(px(2.0))
                                .bg(tick_color),
                        )
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(self.rail_scroll.clone())
        .h(px(frame_h))
        .w_full();

        let can_up = scroll_top > 1.0;
        let can_down = scroll_top < natural_h - frame_h - 1.0;
        let hover_this = this.downgrade();
        let wheel_this = this.downgrade();
        let mut frame = div()
            .id("turn-rail-frame")
            .relative()
            .w(px(RAIL_WIDTH_PX))
            .h(px(frame_h))
            .flex_none()
            .cursor_pointer()
            .on_hover(move |hovered, _, cx| {
                let _ = hover_this.update(cx, |v, cx| {
                    v.rail_pointer_inside = *hovered;
                    if !hovered && v.preview_turn.is_some() {
                        v.preview_turn = None;
                        cx.notify();
                    }
                });
            })
            .on_scroll_wheel(move |_, _, cx| {
                // 梯滚动改变渐隐带/预览卡位置：下一帧重取偏移
                if let Some(v) = wheel_this.upgrade() {
                    v.update(cx, |_, cx| cx.notify());
                }
            })
            .child(marks);
        if can_up {
            frame = frame.child(rail_fade(true));
        }
        if can_down {
            frame = frame.child(rail_fade(false));
        }
        if let Some(p_ix) = self.preview_turn.and_then(|t| items.iter().position(|i| i.turn == t)) {
            let item = &items[p_ix];
            // 预览卡对中于标记（标记位置在滚动梯内：减去滚动偏移），并
            // 夹持在框架两端内（web .preview top clamp）。
            let mark_mid = p_ix as f32 * TURN_SPACING_PX + RAIL_INSET_PX;
            let top = (mark_mid - scroll_top - RAIL_PREVIEW_HEIGHT_PX / 2.0)
                .clamp(0.0, (frame_h - RAIL_PREVIEW_HEIGHT_PX).max(0.0));
            frame = frame.child(
                div()
                    .absolute()
                    .right(px(RAIL_WIDTH_PX + 10.0))
                    .top(px(top))
                    .w(px(RAIL_PREVIEW_WIDTH_PX))
                    .max_h(px(RAIL_PREVIEW_HEIGHT_PX))
                    .overflow_hidden()
                    .v_flex()
                    .px(px(12.0))
                    .py(px(10.0))
                    // web alpha.4：border 撤掉，发丝描边画进 elevation-panel
                    // （描边取默认 l4）
                    .rounded(px(10.0))
                    .bg(theme::t().layer1)
                    .shadow(theme::elevation_panel())
                    .child(
                        div()
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text)
                            .text_ellipsis()
                            .child(item.prompt.clone()),
                    )
                    .when(!item.response.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(4.0))
                                .text_size(px(12.0))
                                .line_height(px(18.0))
                                .text_color(theme::t().caption)
                                // 3×18px 行夹持（web line-clamp: 3）
                                .max_h(px(54.0))
                                .overflow_hidden()
                                .child(item.response.clone()),
                        )
                    }),
            );
        }

        // 外层铺满滚动区做竖直居中（web top: band/2 + translateY(-50%)）；
        // py 32px 对应 web band-64 的框架高度让步。无监听者，不拦列表事件。
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .items_center()
                .justify_end()
                .pr(px(12.0))
                .py(px(32.0))
                .child(frame)
                .into_any_element(),
        )
    }
}

/// 梯可滚端的渐隐带（web mask-image 线性渐隐的近似：向背景色过渡）。
fn rail_fade(top: bool) -> Div {
    let bg = theme::t().bg_base;
    let transparent = Hsla { a: 0.0, ..Hsla::from(bg) };
    let (from, to) = if top {
        (linear_color_stop(transparent, 0.0), linear_color_stop(bg, 1.0))
    } else {
        (linear_color_stop(bg, 0.0), linear_color_stop(transparent, 1.0))
    };
    let d = div().absolute().left_0().w_full().h(px(RAIL_FADE_PX));
    if top {
        d.top_0().bg(linear_gradient(180.0, from, to))
    } else {
        d.bottom_0().bg(linear_gradient(0.0, from, to))
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 内容列宽（AppView 布局产物；读取经 accessed-entities 跟踪，
        // 宿主布局变化时本视图缓存自动失效）
        let width = self.app.read_with(cx, |v, _| v.center_width);
        let content_w = layout::chat_content_width(width);
        // 本帧折叠派生只算一次，render_item 逐可见行经 Rc 读缓存
        *self.folds_frame.borrow_mut() = Rc::new(self.turn_process_folds());

        let this = cx.entity();
        if self.tab == CenterTab::Trajectory {
            // 轨迹通栏（上游 views.module.css .root width:100%），不吃对话
            // 内容列的 680~920 限宽
            return self.render_trajectory(&this, width, cx).into_any_element();
        }

        let chat_list_state = self.chat_list.clone();
        let list_this = this.clone();
        let chat_list_el = gpui::list(chat_list_state.clone(), move |ix, _window, cx| {
            let entry = list_this.read_with(cx, |v, _| v.entries.get(ix).cloned());
            match entry {
                Some(e) => {
                    let this = list_this.clone();
                    list_this
                        .read_with(cx, |v, _| {
                            let folds = v.folds_frame.borrow().clone();
                            // 轮次过程折叠（web turn-process，compact 默认）：
                            // 过程组首条目的槽位渲染控制行；闭合时其余成员
                            // 零高隐藏；答案条目隐藏本步 reasoning 块
                            if let Some(f) = folds.get(&e.turn).copied() {
                                let expanded = v.turn_expanded.contains(&e.turn);
                                let member = matches!(e.role, Role::Assistant | Role::Context)
                                    && f.first_process <= ix
                                    && ix < f.answer;
                                if member {
                                    let t_ctl = this.clone();
                                    if ix == f.first_process {
                                        let mut labels: Vec<String> = Vec::new();
                                        if f.tools > 0 {
                                            labels.push(format!("{} 次工具调用", f.tools));
                                        }
                                        if f.messages > 0 {
                                            labels.push(format!("{} 条消息", f.messages));
                                        }
                                        if f.subagents > 0 {
                                            labels.push(format!("{} 个 subagent", f.subagents));
                                        }
                                        let label = if labels.is_empty() {
                                            "已思考".to_string()
                                        } else {
                                            labels.join(" · ")
                                        };
                                        let control = widgets::turn_process_control(
                                            ix as u64,
                                            &label,
                                            expanded,
                                            move |_, _, cx| {
                                                t_ctl.update(cx, |v, cx| {
                                                    if !v.turn_expanded.remove(&e.turn) {
                                                        v.turn_expanded.insert(e.turn);
                                                    }
                                                    // 成员从零高占位 ↔ 完整条目，该轮
                                                    // 条目高度集体变化：区间失效重测
                                                    v.invalidate_chat_heights_range(f.first_process..f.answer + 1);
                                                    cx.notify();
                                                });
                                            },
                                        );
                                        if expanded {
                                            // 控制行（固定 33px）与展开体必须是
                                            // 兄弟节点（web 同构）：曾把整个条目
                                            // 塞进控制行当 child，内容垂直溢出、
                                            // 叠在后续行上
                                            return div()
                                                .w_full()
                                                .v_flex()
                                                .child(control)
                                                .child(v.render_entry(&e, ix, &this, content_w))
                                                .into_any_element();
                                        }
                                        return control.into_any_element();
                                    }
                                    if !expanded {
                                        // 隐藏成员：零高占位（无 pb，不出 16px 缝）
                                        return div().into_any_element();
                                    }
                                }
                                if ix == f.answer && !expanded {
                                    let mut answer = e.clone();
                                    answer.blocks.retain(|b| !matches!(b, MsgBlock::Reasoning { .. }));
                                    return v.render_entry(&answer, ix, &this, content_w).pb_4().into_any_element();
                                }
                            }
                            // gpui list 无 gap 概念：条目间距用 pb 模拟（web 列 gap 16px）
                            v.render_entry(&e, ix, &this, content_w).pb_4().into_any_element()
                        })
                        .into_any_element()
                }
                None => {
                    let ix = ix;
                    list_this
                        .read_with(cx, |v, _| {
                            if ix == v.entries.len() && v.running {
                                v.render_status_line().into_any_element()
                            } else {
                                // 统计行已移至输入卡上方（web composer.dock 槽）
                                div().into_any_element()
                            }
                        })
                        .into_any_element()
                }
            }
        })
        .size_full()
        .with_sizing_behavior(ListSizingBehavior::Auto)
        // 内容列随中栏宽度自适应、居中（web ChatView .column）
        .max_w(px(content_w))
        .mx_auto();

        let show_jump = !self.chat_near_bottom();
        // 轮次导航栏（0.1.2-alpha.3 turn rail）：全日志 outline + 已加载
        // 锚合并成梯，右缘悬浮。轨迹 tab 在上方分支提前返回。
        let rail_items = self.rail_items();
        let rail = self.render_turn_rail(&this, &rail_items, width);
        let t_jump = this.clone();
        let chat_list_state_for_wheel = self.chat_list.clone();
        let wheel_view = this.clone();
        // 两侧空白：list hitbox 只覆盖内容列（padding 区在命中链之外），
        // 由 wrapper 接住滚轮转发给列表；指针在列表 viewport 内时交给列表
        // 自身，避免双重滚动
        div()
            .id("chat-scroll")
            .h_full()
            .relative()
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, cx| {
                if !chat_list_state_for_wheel.viewport_bounds().contains(&event.position) {
                    let delta = event.delta.pixel_delta(px(20.0));
                    if !delta.y.is_zero() {
                        chat_list_state_for_wheel.scroll_by(-delta.y);
                        wheel_view.update(cx, |_, cx| cx.notify());
                    }
                }
            })
            // web .scroll { padding: 16px … }：滚动区顶部留 16px，首条消息
            // 不与 header 分隔线贴合（曾缺失导致用户气泡顶死分隔线）
            .child(chat_list_el.pt(px(16.0)).px_8())
            .children(rail)
            .when(show_jump, |d| {
                d.child(
                    div()
                        .id("chat-jump-bottom")
                        .absolute()
                        .right(px(24.0))
                        .bottom(px(16.0))
                        .size(px(34.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        // web alpha.4 .scroll：border 撤掉，发丝描边重绑 l3
                        // 画进 elevation-panel
                        .bg(theme::t().surface)
                        .text_color(theme::t().text_2)
                        .cursor_pointer()
                        .shadow(theme::elevation_panel_with(theme::t().border_l3))
                        .hover(|s| s.bg(theme::t().surface_2).text_color(theme::t().text))
                        .tooltip(tip("回到底部"))
                        .on_click(move |_, _, cx| {
                            t_jump.update(cx, |v, _cx| {
                                v.list_bottom.set(true);
                                v.sync_chat_list(true);
                            });
                        })
                        .child(Icon::new(IconName::ChevronDown).size(px(16.0))),
                )
            })
            .into_any_element()
    }
}

// --- footer/统计面板（web TurnUsagePanel / TurnTimePanel） ---------------------

/// 时长格式（web formatDuration 风格：<60s 一位小数秒，否则 分+秒）。
fn fmt_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{}s", secs as u64 / 60, (secs as u64) % 60)
    }
}

/// web StatsLine 文本：组按「 | 」连接，无数据的组整段丢弃——
/// 计数（轮·步）、耗时（LLM/工具调用）、token（缓存命中 + 输入/输出）。
fn stats_line_text(
    turns: u64,
    steps: u64,
    llm: std::time::Duration,
    tool: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
) -> String {
    let mut groups: Vec<String> = Vec::new();
    if steps > 0 {
        groups.push(format!("{turns} 轮 · {steps} 步"));
        let mut durations: Vec<String> = Vec::new();
        if llm.as_secs_f64() > 0.0 {
            durations.push(format!("LLM {}", fmt_duration(llm)));
        }
        if tool.as_secs_f64() > 0.0 {
            durations.push(format!("工具调用 {}", fmt_duration(tool)));
        }
        if !durations.is_empty() {
            groups.push(durations.join(" · "));
        }
    }
    if input_tokens > 0 || output_tokens > 0 {
        // 计费输入 = 总输入 − 缓存读（web billedInputTokens）
        let billed = input_tokens.saturating_sub(cache_read);
        if cache_read > 0 {
            let pct = cache_read as f64 / (cache_read + billed) as f64 * 100.0;
            groups.push(format!("缓存命中 {pct:.0}%"));
        }
        groups.push(format!("输入 {billed} tok · 输出 {output_tokens} tok"));
    }
    groups.join(" | ")
}

/// 助手消息完成后的 footer（web turn tail）：复制按钮（悬停显现）+
/// 「用量」「用时」两个统计药丸（各自点击弹出详情对话框，TurnUsagePanel
/// 同语义；无用量数据的轮次保持纯文本用时行）+ 日历时钟文本。
fn render_entry_footer(
    elapsed: Duration,
    usage: Option<TurnUsage>,
    ended_at_ms: Option<u64>,
    this: &Entity<ChatView>,
    ei: usize,
    latest_turn: bool,
) -> Div {
    let t = this.clone();
    let group: SharedString = format!("assistant-msg-{ei}").into();
    let group_copy = group.clone();

    // 用量药丸 + 对话框（有 token 数据才出现；web 同款）
    let usage_pill = usage.clone().map(|u| {
        gpui_component::popover::Popover::new(("turn-usage-pop", ei as u64))
            .anchor(gpui::Corner::TopLeft)
            .trigger(
                gpui_component::button::Button::new(("turn-usage-btn", ei as u64))
                    .ghost()
                    .child(pill_row(
                        "icons/database.svg",
                        format!("用量 {} tok", format_tokens_compact(u.total_tokens())),
                    )),
            )
            .content(move |_, _, _| turn_usage_panel(u.clone()).into_any_element())
    });

    // 用时药丸 + 对话框（有用量数据时是药丸，否则并入纯文本分支）
    let time_pill = usage.clone().map(|u| {
        gpui_component::popover::Popover::new(("turn-time-pop", ei as u64))
            .anchor(gpui::Corner::TopLeft)
            .trigger(
                gpui_component::button::Button::new(("turn-time-btn", ei as u64))
                    .ghost()
                    .child(pill_row(
                        "icons/clock.svg",
                        format!("用时 {}", format_run_duration(elapsed.as_millis() as u64)),
                    )),
            )
            .content(move |_, _, _| turn_time_panel(u.clone()).into_any_element())
    });

    let plain_time = format!("用时 {}", format_run_duration(elapsed.as_millis() as u64));

    // web MessageIconActions：整行（复制 + 用量/用时药丸 + 时钟）按轮次
    // 新旧显隐——最新一轮常驻，更早的轮悬停条目时显现（opacity 保持布局）
    let mut row = div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .opacity(if latest_turn { 1.0 } else { 0.0 })
        .group_hover(group_copy, |s| s.opacity(1.0))
        .child(
        div()
            .id(("copy-assistant", ei as u64))
            .size(px(20.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .cursor_pointer()
            .text_color(theme::t().caption)
            .hover(|s| s.text_color(theme::t().text_2).bg(theme::t().hover))
            .tooltip(tip("复制"))
            .on_click(move |_, _, cx| {
                t.update(cx, |v, cx| {
                    // 复制本条助手消息全部文本块
                    let mut text = String::new();
                    if let Some(entry) = v.entries.get(ei) {
                        for block in &entry.blocks {
                            if let MsgBlock::Text(t) = block {
                                text.push_str(t);
                            }
                        }
                    }
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                });
            })
            .child(Icon::new(IconName::Copy).size(px(14.0))),
    );
    match (usage_pill, time_pill) {
        (Some(usage_p), Some(time_p)) => {
            row = row.child(usage_p).child(time_p);
        }
        (None, Some(_)) | (None, None) => {
            // 无用量数据：保持纯文本用时行（web 同语义，间距不变）
            row = row.child(plain_text_footer(&plain_time));
        }
        (Some(_), None) => {
            row = row.child(plain_text_footer(&plain_time));
        }
    }
    // 日历时钟文本缀在统计之后（web clock 位置）
    if let Some(ended) = ended_at_ms {
        let clock = format_message_clock(ended, now_ms());
        if !clock.is_empty() {
            row = row.child(plain_text_footer(&clock));
        }
    }
    row
}

/// 药丸行（图标 + 标签，caption 色 12px）。
fn pill_row(icon_path: &'static str, label: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::t().caption)
        .child(
            gpui::svg()
                .path(icon_path)
                .size(px(12.0))
                .text_color(theme::t().caption),
        )
        .child(label)
}

/// 纯文本 footer 段（时钟 / 无用量时的用时）。
fn plain_text_footer(text: &str) -> Div {
    div()
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(theme::t().caption)
        .child(text.to_string())
}

/// 统计对话框骨架（web TurnUsagePanel.module.css 的 panel 面）。
fn turn_stat_panel(
    icon_path: &'static str,
    title: &'static str,
    title_value: Option<String>,
    rows: Vec<(&'static str, String)>,
) -> Div {
    let mut panel = div()
        .w(px(300.0))
        .bg(theme::t().surface)
        // web alpha.4 TurnUsagePanel 弹出卡：border 撤掉，r12，描边重绑 l1
        // 画进 elevation-prominent
        .rounded(px(12.0))
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .shadow(theme::elevation_prominent());
    let mut title_row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::t().text_2)
                .child(
                    gpui::svg()
                        .path(icon_path)
                        .size(px(12.0))
                        .text_color(theme::t().text_2),
                )
                .child(title),
        );
    if let Some(v) = title_value {
        title_row = title_row.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::t().caption)
                .child(v),
        );
    }
    panel = panel.child(title_row);
    // web .titleRule：标题下横线 0.5px l2（alpha.4 hairline 化并校正色阶）
    panel = panel.child(div().w_full().h(px(0.5)).bg(theme::t().border_l2));
    for (label, value) in rows {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::t().caption)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::t().text_2)
                        .child(value),
                ),
        );
    }
    panel
}

/// 本轮用量对话框（web TurnUsagePanel：模型路由 / 缓存命中 / 四桶明细）。
fn turn_usage_panel(u: TurnUsage) -> Div {
    let mut rows = vec![];
    if !u.route.is_empty() {
        rows.push(("提供方 / 模型", u.route.clone()));
    }
    if let Some(hit) = u.cache_hit_percent() {
        rows.push(("缓存命中", format!("{hit}%")));
    }
    rows.push((
        "未缓存输入",
        format!("{} tok", format_tokens_exact(u.uncached_input())),
    ));
    if let Some(read) = u.cache_read {
        rows.push(("缓存读取", format!("{} tok", format_tokens_exact(read))));
    }
    if let Some(write) = u.cache_write {
        rows.push(("缓存写入", format!("{} tok", format_tokens_exact(write))));
    }
    let output = match u.reasoning {
        Some(r) => format!(
            "{} tok（其中推理 {}）",
            format_tokens_exact(u.output_tokens),
            format_tokens_exact(r)
        ),
        None => format!("{} tok", format_tokens_exact(u.output_tokens)),
    };
    rows.push(("输出", output));
    turn_stat_panel(
        "icons/database.svg",
        "本轮用量",
        Some(format!("{} tok", format_tokens_exact(u.total_tokens()))),
        rows,
    )
}

/// 本轮用时对话框（web TurnTimePanel：总用时 / TPS / TTFT）。
fn turn_time_panel(u: TurnUsage) -> Div {
    let mut rows = vec![("本轮总用时", format_run_duration(u.run_ms))];
    if let Some(tps) = u.tokens_per_second() {
        rows.push(("输出速度（TPS）", format!("{} tok/s", format_tps(tps))));
    }
    if let Some(ttft) = u.ttft_ms {
        rows.push((
            "首 token 平均用时（TTFT）",
            format!("{}秒", format_latency_seconds(ttft)),
        ));
    }
    turn_stat_panel("icons/clock.svg", "本轮用时和速度", None, rows)
}

fn attach_tool_result(
    last: Option<&mut ChatEntry>,
    call_id: &str,
    result: &str,
    error: bool,
) {
    if let Some(entry) = last
        && let Some(MsgBlock::Tool(tool)) = entry.blocks.iter_mut().rev().find(|b| matches!(b, MsgBlock::Tool(t) if t.id == call_id))
    {
        tool.result = Some(result.to_string());
        tool.error = error;
    }
}

use dsh_agent_loop::AgentEvent;
use dsh_llm::{ContentBlock, MessageSource};
use dsh_session::{SessionEvent, TurnEndReason};
