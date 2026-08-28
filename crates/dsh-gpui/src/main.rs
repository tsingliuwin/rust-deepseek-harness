//! dsh-gpui — 原生 GPUI 客户端，1:1 对齐参考 dsh Web UI。
//!
//! 布局契约（`packages/client/ui-layout`）：左 sidebar（280px，可折叠成 56px
//! 图标栏，<1024 自动折叠）· 中会话区（≥640px，内容列 748px）· 右 details
//! （360px，300–520 可拖）。视觉 token 见 `theme.rs`（对齐
//! `packages/client/ui-theme` 暗色主题）。会话区 = header（标题 + tab）+
//! 消息流（用户气泡 / Think 折叠行 / 工具行 / markdown）+ 底部胶囊输入卡。

mod assets;
mod theme;

use async_stream::stream;
use async_trait::async_trait;
use dsh_agent_loop::{AgentEvent, AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_fs::FsTool;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, Message, MessageSource, SessionId, StreamChunk,
};
use dsh_llm_deepseek::DeepSeekAdapter;
use dsh_persist::SessionRecorder;
use dsh_session::{Session, SessionEvent};
use dsh_shell::ShellTool;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use dsh_web::WebTool;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    text::{TextView, TextViewStyle},
    Icon, IconName, Root, StyledExt,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- 参考 ui-layout/columns.ts 列宽契约 ---------------------------------------

const SIDEBAR_MIN: f32 = 264.0;
const SIDEBAR_MAX: f32 = 420.0;
const SIDEBAR_DEFAULT: f32 = 280.0;
const SIDEBAR_COLLAPSED: f32 = 56.0;
const SIDEBAR_AUTO_COLLAPSE: f32 = 1024.0;
const CENTER_MIN: f32 = 640.0;
const DETAILS_MIN: f32 = 300.0;
const DETAILS_MAX: f32 = 520.0;
const DETAILS_DEFAULT: f32 = 360.0;

/// 会话内容列宽（ConversationRoot --dsh-chat-content-width）。
const CHAT_CONTENT_WIDTH: f32 = 748.0;
/// 输入卡上限 = 内容列 + 两侧 clearance 16px。
const COMPOSER_CARD_WIDTH: f32 = CHAT_CONTENT_WIDTH + 32.0;
/// 用户气泡宽度上限（MessageItem .userStack）。
const USER_BUBBLE_MAX: f32 = 525.0;

/// columns.ts 的「让步链」。
fn compute_columns(viewport: f32, sidebar_pref: f32, details_pref: f32) -> (f32, f32, f32) {
    let s = if sidebar_pref <= 0.0 {
        SIDEBAR_COLLAPSED
    } else {
        sidebar_pref.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
    };
    let d0 = if details_pref <= 0.0 { 0.0 } else { details_pref.clamp(DETAILS_MIN, DETAILS_MAX) };
    if s + d0 + CENTER_MIN <= viewport {
        return (s, viewport - s - d0, d0);
    }
    let d1 = if d0 == 0.0 { 0.0 } else { (viewport - s - CENTER_MIN).max(DETAILS_MIN) };
    if s + d1 + CENTER_MIN <= viewport {
        return (s, CENTER_MIN, d1);
    }
    (s, (viewport - s).max(0.0), 0.0)
}

// --- 消息块模型 ---------------------------------------------------------------

/// 工具调用块（web 版 ToolRow）。
struct ToolBlock {
    id: String,
    name: String,
    arguments: String,
    result: Option<String>,
    error: bool,
    open: bool,
}

enum MsgBlock {
    Text(String),
    Reasoning { text: String, open: bool },
    Tool(ToolBlock),
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
    Error,
}

struct ChatEntry {
    role: Role,
    blocks: Vec<MsgBlock>,
    done: bool,
    /// 回合用时（TurnStarted → TurnEnded）。
    elapsed: Option<Duration>,
}

/// One session shown in the sidebar list.
#[derive(Clone)]
struct SessionMeta {
    id: SessionId,
    title: String,
}

/// 详情面板当前选中的工具调用。
struct ToolDetail {
    name: String,
    arguments: String,
    result: Option<String>,
    error: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum CenterTab {
    Conversation,
    Trajectory,
}

/// 用 `TextView::markdown` 渲染一段 markdown，样式对齐 web 版
/// MarkdownText.module.css + 字号标尺。
#[derive(IntoElement)]
struct MarkdownBlock {
    text: String,
    id: usize,
}

impl RenderOnce for MarkdownBlock {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut style = TextViewStyle::default().paragraph_gap(rems(1.0));
        style.heading_base_font_size = px(16.0);
        style.heading_font_size = Some(Arc::new(|level, base| match level {
            1 => px(24.0),
            2 => px(22.0),
            3 => px(20.0),
            4 => px(16.0),
            _ => base,
        }));
        style.code_block = StyleRefinement::default()
            .bg(theme::CODE_BG)
            .rounded(px(12.0))
            .p(px(16.0));
        style.is_dark = true;
        TextView::markdown(self.id, self.text, window, cx).style(style)
    }
}

#[derive(Clone, Copy)]
enum DragSide {
    Sidebar,
    Details,
}

#[derive(Clone, Copy)]
struct DragState {
    side: DragSide,
    start_x: f32,
    start_width: f32,
}

// --- 根视图 ------------------------------------------------------------------

/// AppView 的运行期依赖（打包传入以控制构造参数个数）。
struct AppDeps {
    recorder: Arc<SessionRecorder>,
    llm: Arc<LlmRuntime>,
}

struct AppView {
    agent: Arc<ReactLoopAgent>,
    recorder: Arc<SessionRecorder>,
    llm: Arc<LlmRuntime>,
    sessions: Vec<SessionMeta>,
    entries: Vec<ChatEntry>,
    input: Entity<InputState>,
    api_input: Entity<InputState>,
    desired_model: String,
    settings_open: bool,
    pending_clear: bool,
    _input_subscription: Subscription,
    chat_scroll: ScrollHandle,
    // 布局状态
    sidebar_collapsed: bool,
    sidebar_width: f32,
    details_open: bool,
    details_width: f32,
    viewport: f32,
    drag: Option<DragState>,
    // 会话运行态
    running: bool,
    turn_started_at: Option<Instant>,
    stats_turns: u64,
    stats_tools: u64,
    // 中栏 tab + 详情选中
    tab: CenterTab,
    selected_tool: Option<ToolDetail>,
}

impl AppView {
    fn new(
        agent: Arc<ReactLoopAgent>,
        deps: AppDeps,
        sessions: Vec<SessionMeta>,
        input: Entity<InputState>,
        api_input: Entity<InputState>,
        desired_model: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&input, |chat, input, event, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: false }) {
                let text: String = input.read_with(cx, |s, _| s.value().to_string());
                let text = text.trim().to_string();
                if !text.is_empty() {
                    chat.push_user(text.clone());
                    chat.agent.followup(text);
                    chat.pending_clear = true;
                    cx.notify();
                }
            }
        });
        let mut view = Self {
            agent,
            recorder: deps.recorder,
            llm: deps.llm,
            sessions,
            entries: Vec::new(),
            input,
            api_input,
            desired_model,
            settings_open: false,
            pending_clear: false,
            _input_subscription: subscription,
            chat_scroll: ScrollHandle::new(),
            sidebar_collapsed: false,
            sidebar_width: SIDEBAR_DEFAULT,
            details_open: true,
            details_width: DETAILS_DEFAULT,
            viewport: 1280.0,
            drag: None,
            running: false,
            turn_started_at: None,
            stats_turns: 0,
            stats_tools: 0,
            tab: CenterTab::Conversation,
            selected_tool: None,
        };
        view.rebuild_from_session();
        view
    }

    /// Rebuild the transcript from the agent's session log (restore/switch).
    fn rebuild_from_session(&mut self) {
        self.entries.clear();
        self.running = false;
        self.turn_started_at = None;
        self.selected_tool = None;
        let session = self.agent.session();
        let session = session.lock().unwrap();
        for entry in session.entries() {
            match &entry.event {
                SessionEvent::UserMessage(m) => {
                    let text: String = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !text.is_empty() {
                        self.entries.push(ChatEntry {
                            role: Role::User,
                            blocks: vec![MsgBlock::Text(text)],
                            done: true,
                            elapsed: None,
                        });
                    }
                }
                SessionEvent::AssistantMessage { message, .. } => {
                    let mut blocks = Vec::new();
                    for b in &message.content {
                        match b {
                            ContentBlock::Text { text } => blocks.push(MsgBlock::Text(text.clone())),
                            ContentBlock::Reasoning { text } => {
                                blocks.push(MsgBlock::Reasoning { text: text.clone(), open: false })
                            }
                            ContentBlock::ToolCall { id, name, arguments } => {
                                blocks.push(MsgBlock::Tool(ToolBlock {
                                    id: id.0.clone(),
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                    result: None,
                                    error: false,
                                    open: false,
                                }))
                            }
                            _ => {}
                        }
                    }
                    if !blocks.is_empty() {
                        self.entries.push(ChatEntry {
                            role: Role::Assistant,
                            blocks,
                            done: true,
                            elapsed: None,
                        });
                    }
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
                _ => {}
            }
        }
    }

    /// Switch the agent to a persisted session and rebuild the transcript.
    fn switch_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if let Ok(session) = self.recorder.load(&id) {
            self.agent.set_session(session);
            self.rebuild_from_session();
            self.stats_turns = 0;
            self.stats_tools = 0;
            self.tab = CenterTab::Conversation;
            cx.notify();
        }
    }

    /// Create a fresh session and make it current.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        self.agent.set_session(Session::new(id.clone()));
        self.entries.clear();
        self.running = false;
        self.turn_started_at = None;
        self.selected_tool = None;
        self.stats_turns = 0;
        self.stats_tools = 0;
        self.tab = CenterTab::Conversation;
        self.sessions.insert(0, SessionMeta { id, title: "新会话".into() });
        cx.notify();
    }

    fn current_session_id(&self) -> SessionId {
        self.agent.session().lock().unwrap().id.clone()
    }

    fn is_empty_session(&self) -> bool {
        self.entries.is_empty()
    }

    fn push_user(&mut self, text: String) {
        // 会话标题：首条用户消息后自动生成（截断到 30 字符）
        let current = self.current_session_id();
        if let Some(meta) = self.sessions.iter_mut().find(|s| s.id == current)
            && meta.title == "新会话"
            && !text.is_empty()
        {
            meta.title = text.chars().take(30).collect();
        }
        self.entries.push(ChatEntry {
            role: Role::User,
            blocks: vec![MsgBlock::Text(text)],
            done: true,
            elapsed: None,
        });
        self.chat_scroll.scroll_to_bottom();
    }

    fn last_assistant(&mut self) -> &mut ChatEntry {
        let new = !matches!(self.entries.last(), Some(e) if e.role == Role::Assistant && !e.done);
        if new {
            self.entries.push(ChatEntry {
                role: Role::Assistant,
                blocks: Vec::new(),
                done: false,
                elapsed: None,
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
    }

    fn push_reasoning(&mut self, text: &str) {
        let blocks = &mut self.last_assistant().blocks;
        match blocks.last_mut() {
            Some(MsgBlock::Reasoning { text: t, .. }) => t.push_str(text),
            _ => blocks.push(MsgBlock::Reasoning { text: text.to_string(), open: false }),
        }
    }

    fn push_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TurnStarted { .. } => {
                self.running = true;
                self.turn_started_at = Some(Instant::now());
                self.stats_turns += 1;
            }
            AgentEvent::TextDelta { text } => self.push_text(&text),
            AgentEvent::ReasoningDelta { text } => self.push_reasoning(&text),
            AgentEvent::ToolCall { tool_call_id, name, arguments: args } => {
                self.stats_tools += 1;
                self.last_assistant().blocks.push(MsgBlock::Tool(ToolBlock {
                    id: tool_call_id.0,
                    name,
                    arguments: args,
                    result: None,
                    error: false,
                    open: false,
                }));
            }
            AgentEvent::ToolResult { tool_call_id, is_error } => {
                // 实时结果文本从会话日志回读（事件本身只带 id/error）。
                let result = self.latest_tool_result_text(&tool_call_id.0);
                let last = self.entries.last_mut();
                attach_tool_result(last, &tool_call_id.0, result.0.as_str(), is_error);
            }
            AgentEvent::AssistantMessage { .. } => {}
            AgentEvent::TurnEnded { .. } => {
                self.running = false;
                let elapsed = self.turn_started_at.map(|t| t.elapsed());
                if let Some(e) = self.entries.last_mut() {
                    e.done = true;
                    e.elapsed = elapsed;
                }
                self.turn_started_at = None;
            }
            AgentEvent::Error { message, .. } => {
                self.running = false;
                self.turn_started_at = None;
                self.entries.push(ChatEntry {
                    role: Role::Error,
                    blocks: vec![MsgBlock::Text(message)],
                    done: true,
                    elapsed: None,
                });
            }
        }
        self.chat_scroll.scroll_to_bottom();
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

    fn select_tool(&mut self, detail: ToolDetail, cx: &mut Context<Self>) {
        self.selected_tool = Some(detail);
        self.details_open = true;
        cx.notify();
    }

    /// 发送按钮路径：读输入框 → 追加 → 清空（window 由 on_click 闭包提供）。
    fn send_from_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text: String =
            self.input.read_with(cx, |s, _| s.value().to_string()).trim().to_string();
        if text.is_empty() || self.running {
            return;
        }
        self.push_user(text.clone());
        self.agent.followup(text);
        self.input.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    // --- 渲染 ---------------------------------------------------------------

    fn block_element(&self, block: &MsgBlock, ei: usize, bi: usize, this: &Entity<AppView>) -> AnyElement {
        match block {
            MsgBlock::Text(t) => div()
                .w_full()
                .text_color(theme::TEXT)
                .child(MarkdownBlock { text: t.clone(), id: 1_000_000 + ei * 1000 + bi })
                .into_any_element(),

            MsgBlock::Reasoning { text, open } => {
                let open = *open;
                let t = this.clone();
                let mut row = div()
                    .id(("think-row", (ei * 1000 + bi) as u64))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .hover(|s| s.bg(theme::HOVER))
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
                            .text_color(theme::TEXT_2),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::TEXT)
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
                            .text_color(theme::TEXT_3)
                            .child(first_line(text)),
                    );
                if open {
                    row = row.child(
                        div()
                            .pt_1()
                            .pb_1()
                            .pl(px(22.0))
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::TEXT_3)
                            
                            .child(text.clone()),
                    );
                }
                div().w_full().child(row).into_any_element()
            }

            MsgBlock::Tool(tool) => {
                let open = tool.open;
                let (label, icon) = tool_display(&tool.name);
                let t = this.clone();
                let mut row = div()
                    .id(("tool-row", (ei * 1000 + bi) as u64))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .hover(|s| s.bg(theme::HOVER))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(MsgBlock::Tool(tool)) =
                                v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                            {
                                tool.open = !tool.open;
                                v.selected_tool = Some(ToolDetail {
                                    name: tool.name.clone(),
                                    arguments: tool.arguments.clone(),
                                    result: tool.result.clone(),
                                    error: tool.error,
                                });
                                v.details_open = true;
                            }
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(px(12.0))
                            .text_color(theme::TEXT_2),
                    )
                    .child(Icon::new(icon).size(px(14.0)).text_color(theme::TEXT_2))
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::TEXT)
                            .child(label),
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
                            .text_color(if tool.error { theme::ERROR } else { theme::TEXT_3 })
                            .child(first_line(&tool.arguments)),
                    );
                if open {
                    row = row.child(io_card(
                        &tool.arguments,
                        tool.result.as_deref(),
                        tool.error,
                    ));
                }
                div().w_full().child(row).into_any_element()
            }
        }
    }

    fn render_entry(&self, entry: &ChatEntry, ei: usize, this: &Entity<AppView>) -> Div {
        match entry.role {
            Role::User => {
                let text = entry
                    .blocks
                    .first()
                    .map(|b| match b {
                        MsgBlock::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let t = this.clone();
                let bubble_text = text.clone();
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(6.0))
                    .child(
                        div()
                            .max_w(px(USER_BUBBLE_MAX))
                            .rounded(px(22.0))
                            .bg(theme::SURFACE)
                            .px_4()
                            .py(px(10.0))
                            .text_size(px(theme::FONT_BUBBLE))
                            .line_height(px(theme::FONT_BUBBLE_LEADING))
                            .text_color(theme::TEXT)
                            
                            .child(bubble_text),
                    )
                    .child(
                        // 气泡下方的复制按钮（web MessageIconActions）
                        div()
                            .id(("copy-user", ei as u64))
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(theme::CAPTION)
                            .hover(|s| s.text_color(theme::TEXT_2).bg(theme::HOVER))
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
                let mut col = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(16.0));
                for (bi, block) in entry.blocks.iter().enumerate() {
                    col = col.child(self.block_element(block, ei, bi, this));
                }
                if entry.done && let Some(elapsed) = entry.elapsed {
                    col = col.child(render_entry_footer(elapsed, this, ei));
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
                                .bg(theme::ERROR),
                        )
                        .child(
                            div().child(
                                div()
                                    .text_color(theme::ERROR)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("出错了"),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().text_color(theme::TEXT_2).child(text),
                        ),
                )
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
                        .text_color(theme::ACCENT)
                        .child("Deep diving…"),
                )
                .children(self.turn_started_at.map(|t| {
                    div()
                        .ml_2()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(theme::CAPTION)
                        .child(format!("{}秒", t.elapsed().as_secs()))
                })),
        )
    }

    /// 轨迹 tab：全量工具调用台账。
    fn render_trajectory(&self, this: &Entity<AppView>) -> Div {
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ei, entry) in self.entries.iter().enumerate() {
            for (bi, block) in entry.blocks.iter().enumerate() {
                if let MsgBlock::Tool(tool) = block {
                    let (label, icon) = tool_display(&tool.name);
                    let t = this.clone();
                    let name = tool.name.clone();
                    let arguments = tool.arguments.clone();
                    let result = tool.result.clone();
                    let error = tool.error;
                    rows.push(
                        div()
                            .id(("traj", (ei * 1000 + bi) as u64))
                            .flex()
                            .items_center()
                            .h(px(32.0))
                            .px_2()
                            .gap_1p5()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::HOVER))
                            .on_click(move |_, _, cx| {
                                t.update(cx, |v, cx| {
                                    v.selected_tool = Some(ToolDetail {
                                        name: name.clone(),
                                        arguments: arguments.clone(),
                                        result: result.clone(),
                                        error,
                                    });
                                    v.details_open = true;
                                    cx.notify();
                                });
                            })
                            .child(Icon::new(icon).size(px(14.0)).text_color(theme::TEXT_2))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_ROW))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .text_color(theme::TEXT)
                                    .child(label),
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
                                    .text_color(theme::TEXT_3)
                                    .child(first_line(&tool.arguments)),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        let mut col = div().w_full().max_w(px(CHAT_CONTENT_WIDTH)).mx_auto().v_flex().py_4();
        if rows.is_empty() {
            col = col.child(
                div()
                    .py_4()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(theme::TEXT_3)
                    .child("本轮还没有工具调用记录"),
            );
        } else {
            col = col.children(rows);
        }
        col
    }
}

// --- Render ------------------------------------------------------------------

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_clear {
            self.pending_clear = false;
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
        }

        let vw: f32 = window.viewport_size().width.into();
        self.viewport = vw;
        let narrow = vw < SIDEBAR_AUTO_COLLAPSE;
        let collapsed = self.sidebar_collapsed || narrow;
        let sidebar_pref = if collapsed { 0.0 } else { self.sidebar_width };
        let details_pref = if self.details_open { self.details_width } else { 0.0 };
        let (sw, cw, dw) = compute_columns(vw, sidebar_pref, details_pref);
        let has_text = !self.input.read_with(cx, |s, _| s.value().trim().is_empty());

        let this = cx.entity();

        let mut root = div()
            .size_full()
            .h_flex()
            .relative()
            .bg(theme::BG_BASE)
            .text_color(theme::TEXT)
            .child(self.render_sidebar(collapsed, sw, this.clone()));
        if !collapsed {
            root = root.child(drag_handle(DragSide::Sidebar, this.clone()));
        }
        root = root.child(self.render_center(cw, this.clone(), has_text));
        if dw > 0.0 {
            root = root.child(drag_handle(DragSide::Details, this.clone()));
            root = root.child(self.render_details(dw, this.clone()));
        }
        if self.settings_open {
            root = root.child(self.render_settings_overlay(this));
        }
        root
    }
}

impl AppView {
    /// 设置面板：全屏覆盖层（对齐参考 `shell.overlay` 槽位）。
    fn render_settings_overlay(&self, this: Entity<AppView>) -> Div {
        let t_close = this.clone();
        let t_save = this.clone();
        div()
            .absolute()
            .size_full()
            .top_0()
            .left_0()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.5))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(480.0))
                    .v_flex()
                    .gap_3()
                    .p_4()
                    .rounded(px(16.0))
                    .bg(theme::SURFACE)
                    .border_1()
                    .border_color(theme::BORDER_L2)
                    .child(
                        div().h_flex().items_center().justify_between()
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .line_height(px(20.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::TEXT)
                                    .child("设置 · 模型"),
                            )
                            .child(
                                div()
                                    .id("settings-close")
                                    .size(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .text_color(theme::TEXT_2)
                                    .hover(|s| s.bg(theme::HOVER))
                                    .on_click(move |_, _, cx| {
                                        t_close.update(cx, |v, cx| { v.settings_open = false; cx.notify(); });
                                    })
                                    .child(Icon::new(IconName::Close).size(px(16.0))),
                            ),
                    )
                    .child(
                        div().v_flex().gap_1()
                            .child(div().text_size(px(12.0)).line_height(px(18.0)).text_color(theme::TEXT_3).child("DeepSeek API Key"))
                            .child(Input::new(&self.api_input).appearance(false).w_full())
                            .child(div().text_size(px(12.0)).line_height(px(18.0)).text_color(theme::CAPTION).child(
                                "保存后即刻启用 DeepSeek；留空则继续使用当前模型。",
                            )),
                    )
                    .child(
                        div().text_size(px(12.0)).line_height(px(18.0)).text_color(theme::CAPTION)
                            .child(format!("当前路由：{} / {}", "deepseek", self.desired_model)),
                    )
                    .child(
                        div().flex().justify_end().gap_2().child(
                            div()
                                .id("settings-save")
                                .h(px(32.0))
                                .px_4()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(theme::ACCENT)
                                .text_color(gpui::white())
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::ACCENT_HOVER))
                                .on_click(move |_, _, cx| {
                                    t_save.update(cx, |v, cx| {
                                        let key: String = v.api_input.read_with(cx, |s, _| s.value().to_string());
                                        let key = key.trim().to_string();
                                        if !key.is_empty() {
                                            let adapter = DeepSeekAdapter::new(key);
                                            let _ = v.llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter));
                                            let model = std::env::var("DSH_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
                                            v.desired_model = model.clone();
                                            v.agent.set_provider_and_model("deepseek", model);
                                        }
                                        v.settings_open = false;
                                        cx.notify();
                                    });
                                })
                                .child("保存并启用"),
                        ),
                    ),
            )
    }

    fn render_sidebar(&self, collapsed: bool, width: f32, this: Entity<AppView>) -> Div {
        let mut col = div()
            .h_full()
            .w(px(width))
            .flex_none()
            .bg(theme::SIDEBAR_BG)
            .border_r_1()
            .border_color(theme::BORDER_L1);
        if collapsed {
            let t_expand = this.clone();
            let t_new = this.clone();
            let t_settings = this.clone();
            col = col
                .v_flex()
                .items_center()
                .pt(px(18.0))
                .px(px(10.0))
                .pb_1p5()
                .gap_3()
                .child(
                    rail_icon("sb-expand", IconName::PanelLeftOpen, move |_, _, cx| {
                        t_expand.update(cx, |v, cx| { v.sidebar_collapsed = false; cx.notify(); });
                    }),
                )
                .child(
                    rail_icon("sb-new", IconName::Plus, move |_, _, cx| {
                        t_new.update(cx, |v, cx| { v.new_session(cx); });
                    }),
                )
                .child(div().flex_grow())
                .child(
                    rail_icon("sb-settings", IconName::Settings, move |_, _, cx| {
                        t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                    }),
                );
        } else {
            let t_collapse = this.clone();
            let t_new = this.clone();
            let t_settings = this.clone();
            let current_id = self.current_session_id();
            let session_rows: Vec<Stateful<Div>> = self
                .sessions
                .iter()
                .map(|s| {
                    let t = this.clone();
                    let id = s.id.clone();
                    let active = s.id == current_id;
                    session_row(s.title.clone(), active, move |_, _, cx| {
                        let id = id.clone();
                        t.update(cx, |v, cx| { v.switch_session(id, cx); });
                    })
                })                .collect();
            col = col
                .v_flex()
                .px_3()
                .py_1p5()
                .child(
                    // 品牌行：logo + 名字 + HARNESS pill + 折叠按钮（60px 高）
                    div()
                        .h(px(60.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl_1()
                        .child(
                            div().flex_1().min_w_0().flex().items_center().gap_2()
                                .child(div().text_size(px(theme::FONT_BRAND)).child("🐟"))
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_BRAND))
                                        .line_height(px(24.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("DSH"),
                                )
                                .child(
                                    div()
                                        .px_1()
                                        .rounded(px(3.0))
                                        .border_1()
                                        .border_color(theme::BORDER_L2)
                                        .text_color(theme::TEXT_2)
                                        .font_family(theme_mono())
                                        .text_size(px(9.0))
                                        .line_height(px(14.0))
                                        .child("HARNESS"),
                                ),
                        )
                        .child(
                            icon_btn("sb-collapse", IconName::PanelLeftClose, theme::TEXT_2, move |_, _, cx| {
                                t_collapse.update(cx, |v, cx| { v.sidebar_collapsed = true; cx.notify(); });
                            }),
                        ),
                )
                .child(
                    // 新建会话：38px、r12、白 12% 描边（web .newSession）
                    div()
                        .id("sb-new-session")
                        .h(px(38.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .mx_0p5()
                        .mb_2()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(theme::BORDER_L2)
                        .bg(theme::SURFACE)
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::TEXT)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::SURFACE_2))
                        .on_click(move |_, _, cx| {
                            t_new.update(cx, |v, cx| { v.new_session(cx); });
                        })
                        .child(Icon::new(IconName::Plus).size(px(16.0)))
                        .child("新建会话"),
                )
                .child(
                    // 区块头：工作区（36px）
                    div()
                        .h(px(36.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .pl_1()
                        .mb_1()
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::TEXT_3)
                                .child("工作区"),
                        ),
                )
                .child(
                    // 工作区行（folder + 名称）
                    div()
                        .h(px(34.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .rounded(px(8.0))
                        .child(
                            Icon::new(IconName::Folder)
                                .size(px(16.0))
                                .text_color(theme::ACCENT),
                        )
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::TEXT)
                                .child("DSH"),
                        ),
                )
                .child(
                    div()
                        .id("sidebar-list")
                        .flex_grow()
                        .min_h_0()
                        .overflow_y_scroll()
                        .v_flex()
                        .gap_0p5()
                        .children(session_rows),
                )
                .child(
                    // 底部设置
                    div()
                        .id("sb-settings")
                        .h(px(32.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::HOVER))
                        .on_click(move |_, _, cx| {
                            t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                        })
                        .child(Icon::new(IconName::Settings).size(px(16.0)).text_color(theme::TEXT_3))
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::TEXT_2)
                                .child("设置"),
                        ),
                );
        }
        col
    }

    fn render_center(&self, width: f32, this: Entity<AppView>, has_text: bool) -> Div {
        let t = this.clone();
        let mut center = div()
            .h_full()
            .w(px(width))
            .min_w_0()
            .flex_none()
            .v_flex()
            .bg(theme::BG_BASE);

        let show_header = !self.is_empty_session() || self.running;
        if show_header {
            center = center.child(self.render_header(this.clone()));
        }

        // 主体：hero（空会话）/ 对话 / 轨迹
        if self.is_empty_session() && !self.running {
            center = center.child(self.render_hero(this, has_text));
        } else {
            let body = match self.tab {
                CenterTab::Conversation => self.render_chat(&this),
                CenterTab::Trajectory => self.render_trajectory(&this),
            };
            center = center
                .child(
                    div()
                        .id("chat-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.chat_scroll)
                        .px_8()
                        .vertical_scrollbar(&self.chat_scroll)
                        .child(body),
                )
                .child(self.render_composer_area(t, has_text));
        }
        center
    }

    /// 会话 header：标题行 + 对话/轨迹 tab（ConversationRoot .header）。
    fn render_header(&self, this: Entity<AppView>) -> Div {
        let title = self
            .sessions
            .iter()
            .find(|s| s.id == self.current_session_id())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "会话".into());
        let t_details = this.clone();
        div()
            .flex_none()
            .pt_3()
            .pl(px(20.0))
            .pr(px(28.0))
            .border_b_1()
            .border_color(theme::BORDER_L2)
            .child(
                div()
                    .min_h(px(32.0))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::TEXT)
                            .child(title),
                    )
                    .child(
                        div().flex().items_center().gap_1().px_2().h(px(24.0)).rounded(px(12.0))
                            .hover(|s| s.bg(theme::HOVER))
                            .child(Icon::new(IconName::Bot).size(px(12.0)).text_color(theme::TEXT_2))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_TAB))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .text_color(theme::TEXT_2)
                                    .child("标准模式"),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        icon_btn("details-toggle", IconName::PanelRight, theme::TEXT_2, move |_, _, cx| {
                            t_details.update(cx, |v, cx| { v.details_open = !v.details_open; cx.notify(); });
                        }),
                    ),
            )
            .child(self.render_tabs(this))
    }

    /// 对话 / 轨迹 tab 条（13/16 wt500，激活蓝 + 2px 底条）。
    fn render_tabs(&self, this: Entity<AppView>) -> Div {
        let t1 = this.clone();
        let t2 = this;
        let tab = |id: &'static str, label: &'static str, active: bool, t: Entity<AppView>| {
            div()
                .id(id)
                .cursor_pointer()
                .pb(px(11.0))
                .border_b_2()
                .border_color(if active { theme::ACCENT.into() } else { gpui::transparent_black() })
                .text_size(px(theme::FONT_TAB))
                .line_height(px(16.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if active { theme::ACCENT } else { theme::TEXT_3 })
                .hover(|s| s.text_color(theme::TEXT_2))
                .on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| {
                        v.tab = if label == "轨迹" { CenterTab::Trajectory } else { CenterTab::Conversation };
                        cx.notify();
                    });
                })
                .child(label)
        };
        div()
            .mt_1()
            .pl_2()
            .flex()
            .gap(px(36.0))
            .child(tab("tab-chat", "对话", self.tab == CenterTab::Conversation, t1))
            .child(tab("tab-traj", "轨迹", self.tab == CenterTab::Trajectory, t2))
    }

    /// 消息流（748px 内容列 + 16px 项间距，ChatView .column）。
    fn render_chat(&self, this: &Entity<AppView>) -> Div {
        let mut col = div()
            .w_full()
            .max_w(px(CHAT_CONTENT_WIDTH))
            .mx_auto()
            .v_flex()
            .gap_4()
            .py_4()
            .children(
                self.entries.iter().enumerate().map(|(i, e)| self.render_entry(e, i, this)),
            );
        if self.running {
            col = col.child(self.render_status_line());
        }
        div().w_full().child(col)
    }

    /// 底部 composer 区：统计行 + 输入卡。
    fn render_composer_area(&self, this: Entity<AppView>, has_text: bool) -> Div {
        div()
            .flex_none()
            .v_flex()
            .bg(theme::BG_BASE)
            .pb_2()
            .child(if self.stats_turns > 0 {
                div()
                    .mb(px(6.0))
                    .text_center()
                    .text_size(px(theme::FONT_CAPTION))
                    .line_height(px(theme::FONT_CAPTION_LEADING))
                    .text_color(theme::CAPTION)
                    .child(format!(
                        "{} 轮 · {} 次工具调用",
                        self.stats_turns, self.stats_tools
                    ))
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(self.composer_card(this, has_text))
    }

    /// 空会话 hero：标题 + 工作区行 + 居中输入卡（HeroShell）。
    fn render_hero(&self, this: Entity<AppView>, has_text: bool) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(COMPOSER_CARD_WIDTH))
                    .v_flex()
                    .gap_3()
                    .pb(px(32.0))
                    .child(
                        // 标题行：fish + 探索未至之境 + 预览版 badge
                        div().flex().items_center().justify_center().gap_2p5().child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2p5()
                                .text_size(px(theme::FONT_HERO))
                                .line_height(px(theme::FONT_HERO_LEADING))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::TEXT)
                                .child("🐟 探索未至之境")
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .px_1p5()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme::HOVER)
                                        .bg(gpui::rgb(0x283142))
                                        .text_color(theme::TEXT)
                                        .font_family(theme_mono())
                                        .text_size(px(theme::FONT_CAPTION))
                                        .line_height(px(theme::FONT_CAPTION_LEADING))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("预览版"),
                                ),
                        ),
                    )
                    .child(
                        // 工作区行：folder + 目录名 + chevron
                        div()
                            .flex()
                            .items_center()
                            .pl(px(20.0))
                            .gap_1()
                            .child(Icon::new(IconName::FolderClosed).size(px(14.0)).text_color(theme::TEXT))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_TAB))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::TEXT)
                                    .child(workspace_name()),
                            )
                            .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::CAPTION)),
                    )
                    .child(self.composer_card(this, has_text)),
            )
    }

    /// 输入卡（web InputBar .card）：r22、白 6% 描边、850 表面、
    /// 文本在上（16/24），控件行在下（+ / 模式 | 模型 / 发送）。
    fn composer_card(&self, this: Entity<AppView>, has_text: bool) -> Div {
        let running = self.running;

        let left = div()
            .flex()
            .items_center()
            .gap_4()
            .child(icon_btn("composer-add", IconName::Plus, theme::TEXT, |_, _, _| {}))
            .child(
                div()
                    .id("composer-mode")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_2()
                    .rounded(px(8.0))
                    .child(Icon::new(IconName::Eye).size(px(14.0)).text_color(theme::TEXT_2))
                    .child(
                        div()
                            .text_size(px(theme::FONT_TAB))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::TEXT_2)
                            .child("Workspace Write"),
                    )
                    .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::CAPTION)),
            );

        let mut right = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("composer-model")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_2()
                    .rounded(px(8.0))
                    .child(
                        div()
                            .text_size(px(theme::FONT_TAB))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::TEXT_2)
                            .child(self.desired_model.clone()),
                    )
                    .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::CAPTION)),
            );

        let trailing: AnyElement = if running {
            let t_stop = this.clone();
            // 停止按钮：蓝圆 + 白色方块
            div()
                .id("composer-stop")
                .size(px(34.0))
                .rounded_full()
                .bg(theme::ACCENT)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme::ACCENT_HOVER))
                .on_click(move |_, _, cx| {
                    t_stop.update(cx, |v, _cx| {
                        v.agent.cancel();
                    });
                })
                .child(div().size(px(10.0)).rounded(px(2.0)).bg(gpui::white()))
                .into_any_element()
        } else {
            let t_send = this.clone();
            div()
                .id("composer-send")
                .size(px(34.0))
                .rounded_full()
                .bg(theme::ACCENT)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(!has_text, |d| d.opacity(0.4))
                .when(has_text, |d| d.hover(|s| s.bg(theme::ACCENT_HOVER)))
                .on_click(move |_, window, cx| {
                    t_send.update(cx, |v, cx| v.send_from_composer(window, cx));
                })
                .child(Icon::new(IconName::ArrowUp).size(px(18.0)).text_color(gpui::white()))
                .into_any_element()
        };
        right = right.child(trailing);

        div()
            .w_full()
            .max_w(px(COMPOSER_CARD_WIDTH))
            .mx_auto()
            .flex_none()
            .v_flex()
            .gap_3()
            .pt_2p5()
            .rounded(px(22.0))
            .border_1()
            .border_color(theme::BORDER_L1)
            .bg(theme::SURFACE)
            .child(div().pl_4().pr_3().pt_1().child(Input::new(&self.input).appearance(false).w_full()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .pt(px(2.0))
                    .pb(px(6.0))
                    .child(left)
                    .child(right),
            )
    }

    /// 右侧详情面板（DetailsPanel）。
    fn render_details(&self, width: f32, this: Entity<AppView>) -> Div {
        let t = this.clone();
        let mut body = div().id("details-body").flex_1().min_h_0().overflow_y_scroll().px_4().py_3();
        match &self.selected_tool {
            None => {
                body = body.child(
                    div()
                        .py_2()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .text_color(theme::TEXT_3)
                        .child("点击消息流中的工具行查看详情"),
                );
            }
            Some(tool) => {
                body = body
                    .child(detail_section("工具", div().text_color(theme::TEXT).child(tool.name.clone())))
                    .child(detail_section(
                        "输入",
                        code_card(&first_line(&tool.arguments), false),
                    ))
                    .child(detail_section(
                        "输出",
                        match &tool.result {
                            Some(r) => code_card(r, tool.error),
                            None => div()
                                .text_size(px(13.0))
                                .line_height(px(20.0))
                                .text_color(theme::CAPTION)
                                .child("（运行中…）"),
                        },
                    ));
            }
        }
        div()
            .h_full()
            .w(px(width))
            .flex_none()
            .v_flex()
            .bg(theme::BG_BASE)
            .border_l_1()
            .border_color(theme::BORDER_L2)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pt(px(14.0))
                    .px_3()
                    .pb_3()
                    .border_b_1()
                    .border_color(theme::BORDER_L2)
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::TEXT)
                            .child("详情"),
                    )
                    .child(
                        icon_btn("details-close", IconName::Close, theme::TEXT_2, move |_, _, cx| {
                            t.update(cx, |v, cx| { v.details_open = false; cx.notify(); });
                        }),
                    ),
            )
            .child(body)
    }
}

// --- 小组件 / 帮手 -----------------------------------------------------------

/// 一个可拖拽的列宽把手（透明 8px 热区，web AppFrame .handle）。
fn drag_handle(side: DragSide, this: Entity<AppView>) -> Div {
    let down = this.clone();
    let mv = this.clone();
    let up = this.clone();
    div()
        .w(px(8.0))
        .h_full()
        .flex_none()
        .mx(px(-4.0))
        .cursor_col_resize()
        .on_mouse_down(gpui::MouseButton::Left, move |ev, _window, cx| {
            let start_x: f32 = ev.position.x.into();
            down.update(cx, |v, cx| {
                let start_width = match side {
                    DragSide::Sidebar => v.sidebar_width,
                    DragSide::Details => v.details_width,
                };
                v.drag = Some(DragState { side, start_x, start_width });
                cx.notify();
            });
        })
        .on_mouse_move(move |ev, _window, cx| {
            let x: f32 = ev.position.x.into();
            mv.update(cx, |v, cx| {
                if let Some(d) = v.drag {
                    let delta = x - d.start_x;
                    match d.side {
                        DragSide::Sidebar => {
                            v.sidebar_width = (d.start_width + delta).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                        }
                        DragSide::Details => {
                            v.details_width = (d.start_width - delta).clamp(DETAILS_MIN, DETAILS_MAX);
                        }
                    }
                    cx.notify();
                }
            });
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_ev, _window, cx| {
            up.update(cx, |v, cx| {
                v.drag = None;
                cx.notify();
            });
        })
}

/// 28px 圆形图标按钮（web .iconButton）。
fn icon_btn(
    id: &'static str,
    icon: IconName,
    color: impl Into<Hsla>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(color)
        .cursor_pointer()
        .hover(|s| s.bg(theme::HOVER))
        .on_click(on_click)
        .child(Icon::new(icon).size(px(16.0)))
}

/// 折叠栏 36×36 图标钮（web rail .iconButton）。
fn rail_icon(
    id: &'static str,
    icon: IconName,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(36.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .text_color(theme::TEXT)
        .cursor_pointer()
        .hover(|s| s.bg(theme::HOVER))
        .on_click(on_click)
        .child(Icon::new(icon).size(px(18.0)))
}

/// 侧栏会话行（web .sessionRow：32px、r8、选中/hover 白 8%）。
fn session_row(
    title: String,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = format!("session-{title}").into();
    div()
        .id(id)
        .h(px(32.0))
        .flex()
        .items_center()
        .pl(px(16.0))
        .pr_2()
        .gap_1()
        .rounded(px(8.0))
        .cursor_pointer()
        .map(|d| if active { d.bg(theme::HOVER) } else { d })
        .hover(|s| s.bg(theme::HOVER))
        .on_click(on_click)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(20.0))
                .text_color(theme::TEXT)
                .child(title),
        )
}

/// 行内 2×2 分隔点（web .sep）。
fn dot_sep() -> Div {
    div().size(px(2.0)).rounded(px(1.0)).bg(theme::CAPTION).mx_2()
}

/// 工具行 / 输入输出卡（web ToolRow .ioCard）。
fn io_card(input: &str, output: Option<&str>, error: bool) -> Div {
    let mut card = div()
        .ml_1()
        .mt_1()
        .mb_1()
        .v_flex()
        .rounded(px(12.0))
        .border_1()
        .border_color(theme::BORDER_L1)
        .bg(theme::CODE_BG);
    card = card.child(io_section("输入", input, false));
    if let Some(out) = output {
        card = card.child(div().h(px(1.0)).w_full().bg(theme::BORDER_L2));
        card = card.child(io_section("输出", out, error));
    }
    card
}

fn io_section(label: &str, text: &str, error: bool) -> Div {
    div()
        .v_flex()
        .px_4()
        .py_3()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(theme::CAPTION)
                .font_family(theme_mono())
                .child(label.to_string()),
        )
        .child(
            div()
                .font_family(theme_mono())
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(if error { theme::ERROR } else { theme::TEXT_2 })
                .child(text.to_string()),
        )
}

/// 详情面板的一个 section（label + 内容）。
fn detail_section(label: &str, content: Div) -> Div {
    div()
        .mb_4()
        .child(
            div()
                .mb_1p5()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::TEXT_2)
                .child(label.to_string()),
        )
        .child(content)
}

/// 详情面板代码卡（web .code：r12、pad 16、mono 13/22）。
fn code_card(text: &str, error: bool) -> Div {
    div()
        .p_4()
        .rounded(px(12.0))
        .bg(theme::CODE_BG)
        .font_family(theme_mono())
        .text_size(px(13.0))
        .line_height(px(22.0))
        .text_color(if error { theme::ERROR } else { theme::TEXT })
        
        
        .child(text.to_string())
}

/// 助手消息完成后的 footer：复制按钮 + 用时。
fn render_entry_footer(elapsed: Duration, this: &Entity<AppView>, ei: usize) -> Div {
    let t = this.clone();
    let secs = elapsed.as_secs();
    let text = if secs >= 60 {
        format!("用时 {}分{}秒", secs / 60, secs % 60)
    } else {
        format!("用时 {}秒", secs)
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(("copy-assistant", ei as u64))
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .cursor_pointer()
                .text_color(theme::CAPTION)
                .hover(|s| s.text_color(theme::TEXT_2).bg(theme::HOVER))
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
        )
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(theme::CAPTION)
                .child(text),
        )
}

/// 工具显示名 + 图标。
fn tool_display(name: &str) -> (String, IconName) {
    match name {
        "shell" => ("Shell".into(), IconName::SquareTerminal),
        "fs" => ("Fs".into(), IconName::File),
        "web_fetch" => ("Web".into(), IconName::Globe),
        other => (other.to_string(), IconName::Bot),
    }
}

/// 多行文本取首行（截断 120 字符）。
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    line.chars().take(120).collect()
}

/// 当前目录名，hero 工作区行用。
fn workspace_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "workspace".into())
}

fn theme_mono() -> SharedString {
    "Consolas".into()
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

// --- 启动 --------------------------------------------------------------------

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    let events = EventBus::new();
    // agent/request-error 退避重试（参考 dsh-llm-retry 的角色）
    let _retry_disposer = dsh_agent_loop::retry::attach_retry(&events);
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let (provider, model) = match DeepSeekAdapter::from_env() {
        Some(adapter) => {
            let _h = llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter)).expect("register deepseek");
            let model = std::env::var("DSH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());
            ("deepseek".to_string(), model)
        }
        None => {
            let _h = llm.register_adapter(&["mock".to_string()], Arc::new(MockAdapter)).expect("register mock");
            ("mock".to_string(), "mock".to_string())
        }
    };

    let tools = Arc::new(ToolRegistry::new());
    let _fs = tools.register(Arc::new(FsTool)).unwrap();
    let _shell = tools.register(Arc::new(ShellTool)).unwrap();
    let _web = tools.register(Arc::new(WebTool::new())).unwrap();
    let prompt = Arc::new(SystemPrompt::new());
    let demo_prompt = std::env::var("DSH_PROMPT").ok().filter(|s| !s.trim().is_empty());

    // --- 会话持久化：恢复最近的会话，或新建 ---
    let sessions_dir = std::env::var("DSH_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("dsh-rust").join("sessions"))
                .unwrap_or_else(|_| PathBuf::from("dsh-rust-sessions"))
        });
    let recorder = Arc::new(SessionRecorder::new(sessions_dir));
    let existing = recorder.list().unwrap_or_default();
    let (initial_session, sessions_meta, is_fresh) = if let Some(id) = existing.first() {
        let meta: Vec<SessionMeta> = existing
            .iter()
            .map(|sid| {
                let title = recorder
                    .load(sid)
                    .ok()
                    .and_then(|s| s.first_user_text())
                    .map(|t| t.chars().take(30).collect())
                    .unwrap_or_else(|| "新会话".into());
                SessionMeta { id: sid.clone(), title }
            })
            .collect();
        let session = recorder.load(id).unwrap_or_else(|_| Session::new(id.clone()));
        let is_fresh = session.entries().is_empty();
        (session, meta, is_fresh)
    } else {
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        (Session::new(id.clone()), vec![SessionMeta { id, title: "新会话".into() }], true)
    };

    let agent = ReactLoopAgent::new(
        initial_session.id.clone(),
        AgentOptions {
            provider: provider.clone(),
            model: model.clone(),
            max_tokens: None,
            system_prompt: Some("You are DeepSeek Harness (Rust), a helpful coding agent.".into()),
        },
        Arc::clone(&llm),
        tools,
        prompt,
        events,
    );
    agent.set_session(initial_session);

    // 持久化：每个追加的会话事件写入 JSONL
    let recorder_sink = Arc::clone(&recorder);
    let agent_sink = Arc::clone(&agent);
    agent.set_event_sink(move |event| {
        let id = agent_sink.session().lock().unwrap().id.clone();
        let _ = recorder_sink.append(&id, &event);
    });

    let event_rx = agent.subscribe();
    agent.spawn();

    Application::new()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
        gpui_component::init(cx);
        theme::init(cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), &*cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let input = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx)
                        .placeholder("给智能体发消息 (Enter 发送 · Shift+Enter 换行)")
                        .multi_line(true)
                        .auto_grow(1, 14)
                });
                let api_input = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("sk-… (DeepSeek API Key)")
                });
                let desired_model = std::env::var("DSH_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
                let app = cx.new(|cx| {
                    let deps = AppDeps { recorder: Arc::clone(&recorder), llm: Arc::clone(&llm) };
                    AppView::new(
                        Arc::clone(&agent),
                        deps,
                        sessions_meta.clone(),
                        input.clone(),
                        api_input,
                        desired_model,
                        cx,
                    )
                });

                let view = app.clone();
                cx.spawn(move |cx: &mut AsyncApp| {
                    let mut cx = cx.clone();
                    async move {
                        while let Ok(ev) = event_rx.recv_async().await {
                            let _ = cx.update_entity(&view, |app: &mut AppView, cx: &mut Context<AppView>| {
                                app.push_event(ev);
                                cx.notify();
                            });
                        }
                    }
                })
                .detach();

                if is_fresh
                    && let Some(demo) = demo_prompt.as_deref() {
                        Arc::clone(&agent).followup(demo);
                    }

                cx.new(|cx| Root::new(app, window, cx))
            },
        )
        .expect("failed to open window");
    });
}

fn message_text(m: &Message) -> String {
    m.content.iter().filter_map(|b| match b {
        ContentBlock::Text { text } | ContentBlock::Reasoning { text } => Some(text.as_str()),
        _ => None,
    }).collect()
}

struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock (无 API key)".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let last_user = options.messages.iter().rev().find(|m| matches!(m.source, MessageSource::User)).map(message_text).unwrap_or_default();
        let reply = format!(
            "你刚才说的是：**{last_user}**。\n\n## 这是 markdown 渲染演示\n\n- **加粗**文本\n- `行内代码`\n- 有序列表\n\n1. 第一项\n2. 第二项\n\n> 引用块（设置 DEEPSEEK_API_KEY 后可接入真实模型）。"
        );
        let signal = options.signal.clone();
        Ok(Box::pin(stream! {
            yield StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Reasoning };
            yield StreamChunk::ReasoningDelta { index: 0, text: "让我梳理一下要点，再组织回答…".into() };
            yield StreamChunk::BlockStart { index: 1, block_type: ContentBlockType::Text };
            for word in reply.split_inclusive(' ') {
                if signal.aborted() { yield StreamChunk::stream_aborted(); return; }
                yield StreamChunk::TextDelta { index: 1, text: word.to_string() };
                tokio::time::sleep(Duration::from_millis(4)).await;
            }
            yield StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None };
        }))
    }
}
