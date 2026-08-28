//! 通用小件：Markdown 渲染块、图标按钮、tooltip、卡片、行内分隔点等。
//!
//! 这些组件不依赖 AppView（回调一律经泛型闭包注入），可以被任何面板复用。

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    text::{TextView, TextViewStyle},
    tooltip::Tooltip,
    Icon, IconName, StyledExt,
};

use crate::theme;

/// 用 `TextView::markdown` 渲染一段 markdown，样式对齐 web 版
/// MarkdownText.module.css + 字号标尺。
#[derive(IntoElement)]
pub(crate) struct MarkdownBlock {
    pub(crate) text: String,
    pub(crate) id: usize,
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
        TextView::markdown(self.id, self.text, window, cx)
            .style(style)
            .code_block_actions(|code, _window, _cx| {
                // 语言 + 复制 浮标（web CodeBlock .banner 的浮层形态）
                let lang = code.lang().unwrap_or_else(|| "text".into());
                let text = code.code();
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&text, &mut hasher);
                let copy_id: SharedString = format!("md-copy-{:x}", std::hash::Hasher::finish(&hasher)).into();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .line_height(px(14.0))
                            .font_family(theme_mono())
                            .text_color(theme::CAPTION)
                            .child(lang),
                    )
                    .child(
                        div()
                            .id(copy_id)
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(theme::TEXT_3)
                            .hover(|s| s.text_color(theme::TEXT).bg(theme::HOVER))
                            .tooltip(tip("复制代码"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
                            })
                            .child(Icon::new(IconName::Copy).size(px(12.0))),
                    )
                    .into_any_element()
            })
    }
}
/// hover 提示（gpui-component Tooltip）。
pub(crate) fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_window, cx| cx.new(|_| Tooltip::new(text)).into()
}

/// 28px 圆形图标按钮（web .iconButton）。
pub(crate) fn icon_btn(
    id: &'static str,
    icon: IconName,
    color: impl Into<Hsla>,
    tooltip: &'static str,
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
        .tooltip(tip(tooltip))
        .on_click(on_click)
        .child(Icon::new(icon).size(px(16.0)))
}

/// 折叠栏 36×36 图标钮（web rail .iconButton）。
pub(crate) fn rail_icon(
    id: &'static str,
    icon: IconName,
    tooltip: &'static str,
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
        .tooltip(tip(tooltip))
        .on_click(on_click)
        .child(Icon::new(icon).size(px(18.0)))
}
/// 侧栏会话行（web .sessionRow：32px、r8、选中/hover 白 8%，
/// 右侧时间 hover 时切换为「…」操作钮）。
pub(crate) fn session_row(
    index: usize,
    title: String,
    time_label: String,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = format!("session-{title}-{index}").into();
    let group: SharedString = format!("session-row-{index}").into();
    let group_time = group.clone();
    let group_more = group.clone();
    div()
        .id(id)
        .group(group)
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
        .when(!time_label.is_empty(), |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("session-time-{index}")))
                    .text_size(px(12.0))
                    .line_height(px(20.0))
                    .text_color(theme::TEXT_3)
                    .group_hover(group_time, |s| s.opacity(0.0))
                    .child(time_label),
            )
        })
        .child(
            // hover 显现的「…」（web 会话行 hover 切换）
            div()
                .id(SharedString::from(format!("session-more-{index}")))
                .size(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.0)
                .group_hover(group_more, |s| s.opacity(1.0))
                .child(Icon::new(IconName::Ellipsis).size(px(14.0)).text_color(theme::TEXT_3)),
        )
}
/// 行内 2×2 分隔点（web .sep）。
pub(crate) fn dot_sep() -> Div {
    div().size(px(2.0)).rounded(px(1.0)).bg(theme::CAPTION).mx_2()
}
/// 工具行展开的输入/输出卡（web ToolRow .ioCard：r12、每节上限 150px 内滚动）。
pub(crate) fn io_card(uid: u64, input: &str, output: Option<&str>, error: bool) -> Div {
    let mut card = div()
        .ml_1()
        .mt_1()
        .mb_1()
        .v_flex()
        .rounded(px(12.0))
        .border_1()
        .border_color(theme::BORDER_L1)
        .bg(theme::CODE_BG);
    card = card.child(io_section(uid * 2, "输入", input, false));
    if let Some(out) = output {
        card = card.child(div().h(px(1.0)).w_full().bg(theme::BORDER_L2));
        card = card.child(io_section(uid * 2 + 1, "输出", out, error));
    }
    card
}
pub(crate) fn io_section(uid: u64, label: &str, text: &str, error: bool) -> Div {
    // web ToolRow .ioSection：max-content 槽道标签 + 1fr 文本，gap 14，pad 12/16
    div()
        .flex()
        .items_start()
        .gap(px(14.0))
        .px_4()
        .py_3()
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
                .id(("io-scroll", uid))
                .flex_1()
                .min_w_0()
                .max_h(px(150.0))
                .overflow_y_scroll()
                .font_family(theme_mono())
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(if error { theme::ERROR } else { theme::TEXT_2 })
                .child(text.to_string()),
        )
}
/// 详情面板的一个 section（label + 内容）。
pub(crate) fn detail_section(label: &str, content: Div) -> Div {
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
pub(crate) fn code_card(text: &str, error: bool) -> Div {
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
/// 工具显示名 + 图标。
pub(crate) fn tool_display(name: &str) -> (String, IconName) {
    match name {
        "shell" => ("Shell".into(), IconName::SquareTerminal),
        "fs" => ("Fs".into(), IconName::File),
        "web_fetch" => ("Web".into(), IconName::Globe),
        other => (other.to_string(), IconName::Bot),
    }
}

/// 多行文本取首行（截断 120 字符）。
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    line.chars().take(120).collect()
}

/// 当前目录名，hero 工作区行用。
pub(crate) fn workspace_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "workspace".into())
}

pub(crate) fn theme_mono() -> SharedString {
    "Consolas".into()
}
