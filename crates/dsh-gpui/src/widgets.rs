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

/// markdown 拆分：文本段 + 围栏代码段。
enum MdSegment {
    Text(String),
    Code { lang: String, code: String },
}

/// 按 ``` 围栏拆分（粗粒度但稳定；未闭合围栏回退为文本）。
fn split_markdown(md: &str) -> Vec<MdSegment> {
    let newline = "
";
    let mut segments = Vec::new();
    let mut text_buf = String::new();
    let mut lines = md.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if !text_buf.trim().is_empty() {
                segments.push(MdSegment::Text(text_buf.clone()));
                text_buf.clear();
            }
            let lang = trimmed.trim_start_matches("```").trim().to_string();
            let mut code = String::new();
            let mut closed = false;
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                code.push_str(inner);
                code.push_str(newline);
            }
            if !closed {
                text_buf.push_str(line);
                text_buf.push_str(newline);
                text_buf.push_str(&code);
                continue;
            }
            segments.push(MdSegment::Code {
                lang: if lang.is_empty() { "text".into() } else { lang },
                code: code.trim_end_matches(newline).to_string(),
            });
        } else {
            text_buf.push_str(line);
            text_buf.push_str(newline);
        }
    }
    if !text_buf.trim().is_empty() {
        segments.push(MdSegment::Text(text_buf));
    }
    segments
}

/// 代码块完整卡片（web CodeBlock .block/.banner/.pre 对齐）：
/// banner = 语言 mono 标签 + 复制按钮，content = 代码本体。
fn code_block_card(uid: usize, lang: &str, code: &str) -> Div {
    let text = code.to_string();
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&text, &mut hasher);
    let copy_id: SharedString =
        format!("code-copy-{uid}-{:x}", std::hash::Hasher::finish(&hasher)).into();
    let lang_display = if lang.is_empty() { "text".to_string() } else { lang.to_string() };
    div()
        .v_flex()
        .rounded(px(12.0))
        .overflow_hidden()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .px(px(14.0))
                .py(px(9.0))
                .bg(theme::t().code_banner)
                .child(
                    div()
                        .font_family(theme_mono())
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(theme::t().text_3)
                        .child(lang_display),
                )
                .child(
                    div()
                        .id(copy_id)
                        .flex()
                        .items_center()
                        .gap_1()
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_color(theme::t().text_3)
                        .hover(|s| s.text_color(theme::t().text).bg(theme::t().hover))
                        .tooltip(tip("复制代码"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
                        })
                        .child(Icon::new(IconName::Copy).size(px(12.0)))
                        .child(div().text_size(px(11.0)).line_height(px(14.0)).child("复制")),
                ),
        )
        .child(
            div()
                .w_full()
                .p_4()
                .bg(theme::t().code_bg)
                .font_family(theme_mono())
                .text_size(px(13.0))
                .line_height(px(22.0))
                .text_color(theme::t().text)
                .child(code.to_string()),
        )
}

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
        style.is_dark = true;

        let segments = split_markdown(&self.text);
        let mut col = div().v_flex().gap(px(12.0));
        for (i, seg) in segments.into_iter().enumerate() {
            match seg {
                MdSegment::Text(text) => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    col = col.child(
                        TextView::markdown(self.id * 1000 + i * 2, text, window, cx)
                            .style(style.clone()),
                    );
                }
                MdSegment::Code { lang, code } => {
                    col = col.child(code_block_card(self.id * 1000 + i * 2 + 1, &lang, &code));
                }
            }
        }
        col
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
        .hover(|s| s.bg(theme::t().hover))
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
        .text_color(theme::t().text)
        .cursor_pointer()
        .hover(|s| s.bg(theme::t().hover))
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
    blank: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_more: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
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
        .map(|d| if active { d.bg(theme::t().hover) } else { d })
        .hover(|s| s.bg(theme::t().hover))
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
                .text_color(theme::t().text)
                .child(title),
        )
        // 空白会话行（web row.blank）：时间与 … 菜单都不渲染——
        // 行动词对不存在的内容无意义
        .when(!time_label.is_empty() && !blank, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("session-time-{index}")))
                    .text_size(px(12.0))
                    .line_height(px(20.0))
                    .text_color(theme::t().text_3)
                    .group_hover(group_time, |s| s.opacity(0.0))
                    .child(time_label),
            )
        })
        .when(!blank, |d| {
            d.child(
                // hover 显现的「…」（web 会话行 hover 切换：time 让位给菜单钮）
                div()
                    .id(SharedString::from(format!("session-more-{index}")))
                    .size(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .opacity(0.0)
                    .group_hover(group_more, |s| s.opacity(1.0))
                    .hover(|st| st.bg(theme::t().active))
                    .cursor_pointer()
                    // 「…」不是行本身：阻断冒泡，避免同时触发行点击（切换会话）
                    .on_click(move |click, window, cx| {
                        cx.stop_propagation();
                        on_more(click, window, cx);
                    })
                    .child(Icon::new(IconName::Ellipsis).size(px(14.0)).text_color(theme::t().text_3)),
            )
        })
}
/// 行内 2×2 分隔点（web .sep）。
pub(crate) fn dot_sep() -> Div {
    div().size(px(2.0)).rounded(px(1.0)).bg(theme::t().caption).mx_2()
}

/// 工具行状态点（web StateDot：外层 10% 光晕 + 60% 实心内核，
/// 颜色由状态语义决定）。
pub(crate) fn state_dot(color: gpui::Rgba) -> Div {
    div()
        .size(px(8.0))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(gpui::Rgba { r: color.r, g: color.g, b: color.b, a: 0.10 }),
        )
        .child(
            div()
                .absolute()
                .inset(px(1.5))
                .rounded_full()
                .bg(color),
        )
}

/// 运行中的行扫光（web .row::after：300px 带自左滑向右，2.6s ease-out +
/// 10% 尾停后循环；左端渐变 = bg_base 60% 透明）。行容器需
/// relative + overflow_hidden，扫光为其最后 child。
pub(crate) fn row_sweep(elapsed_ms: u64, width: f32) -> Div {
    const PERIOD_MS: u64 = 2600;
    const BAND: f32 = 300.0;
    let t = (elapsed_ms % PERIOD_MS) as f32 / PERIOD_MS as f32;
    // CSS keyframes：0→-300，90% 已到右端（此后保持到 100% 循环）
    let p = (t / 0.9).min(1.0);
    let eased = 1.0 - (1.0 - p) * (1.0 - p); // ease-out 近似
    let left = -BAND + (width + BAND) * eased;
    let base = theme::t().bg_base;
    let peak = gpui::Rgba { r: base.r, g: base.g, b: base.b, a: 0.6 };
    // gpui linear_gradient 仅两个 stop：左右两半各一条渐变合成中峰（≈css 55%）
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(left))
        .w(px(BAND))
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(BAND / 2.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(gpui::transparent_black(), 0.0),
                    gpui::linear_color_stop(peak, 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(BAND / 2.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(peak, 0.0),
                    gpui::linear_color_stop(gpui::transparent_black(), 1.0),
                )),
        )
}
/// 展开体行数上限（web CHAT_READ/DIFF_MAX_LINES = 8：头 4 + 尾 4，
/// 中段以「… 其余 N 行」折叠钮开合）。
const CARD_MAX_LINES: usize = 8;

/// 轮次过程折叠控制行（web TurnProcessNodeView .root：全宽 33px、
/// 底部 l2 分隔线、label 14/24 secondary 省略 + chevron 16 tertiary
/// （闭→右/开→下，web 为 -90°→0° 旋转同观感）、闭合时下距 8px）。
pub(crate) fn turn_process_control(
    uid: u64,
    label: &str,
    open: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(("turn-process", uid))
        .w_full()
        .h(px(33.0))
        .pb(px(8.0))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme::t().border_l2)
        .text_color(theme::t().text_2)
        .cursor_pointer()
        .when(!open, |d| d.mb(px(8.0)))
        .on_click(on_toggle)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(14.0))
                .line_height(px(24.0))
                .child(label.to_string()),
        )
        .child(
            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .size(px(16.0))
                .text_color(theme::t().text_3),
        )
}

/// 折叠行（web FoldToggle + .expand：左对齐、tertiary、hover secondary）。
/// 点击回调由持有状态的渲染站点注入。
pub(crate) fn fold_toggle(
    uid: u64,
    hidden: usize,
    expanded: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let label = if expanded {
        "收起".to_string()
    } else {
        format!("… 其余 {hidden} 行")
    };
    div()
        .id(("tool-fold", uid))
        .w_full()
        .cursor_pointer()
        .text_color(theme::t().text_3)
        .hover(|s| s.text_color(theme::t().text_2))
        .on_click(on_toggle)
        .child(label)
}

/// 终端卡（web TerminalBlock .block/.header/.output，行内绑定：
/// mono 12/18、banner 上限 150 内滚动、输出上限 224 内滚动、gutter 30px
/// 状态点列）。running 只画 banner；settled 有 l2 分隔线 + 输出（空输出
/// 画「无输出」占位）。dsh-shell 无退出码元数据，状态只由点色承载：
/// 运行 accent、成功 green、失败 error。
pub(crate) fn terminal_card(
    uid: u64,
    command: &str,
    cwd: &str,
    output: Option<&str>,
    running: bool,
    error: bool,
) -> Div {
    let dot_color = if running {
        theme::t().accent
    } else if error {
        theme::t().error
    } else {
        theme::t().green
    };
    let cwd_label = prompt_label(cwd);
    let body = command.strip_suffix('\n').unwrap_or(command);
    let command_lines: Vec<&str> = if body.is_empty() { vec![""] } else { body.split('\n').collect() };
    // prompt 行（cwd 标注整次调用，只在首行出现；后续行裸 `$` 对齐）
    let mut prompt = div().v_flex().min_w_0().flex_1();
    for (i, line) in command_lines.iter().enumerate() {
        prompt = prompt.child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .min_w_0()
                .line_height(px(18.0))
                .child(
                    div()
                        .flex_none()
                        .font_family(theme_mono())
                        .text_size(px(12.0))
                        .text_color(theme::t().text_3)
                        .child(if i == 0 { cwd_label.clone() } else { "$".to_string() }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family(theme_mono())
                        .text_size(px(12.0))
                        .text_color(theme::t().text)
                        .child(line.to_string()),
                ),
        );
    }
    // banner：settled 且有非空输出时附「复制」（复制原始输出）
    let mut banner = div()
        .id(("term-banner", uid))
        .relative()
        .flex()
        .items_start()
        .gap_3()
        .pt(px(9.0))
        .pr(px(14.0))
        .pb(px(9.0))
        .pl(px(30.0))
        .max_h(px(150.0))
        .overflow_y_scroll()
        .child(prompt);
    if !running
        && let Some(text) = output
        && !text.trim().is_empty()
    {
        let raw = text.to_string();
        banner = banner.child(
            div()
                .id(("term-copy", uid))
                .flex_none()
                .cursor_pointer()
                .font_family(theme_mono())
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(theme::t().text_2)
                .hover(|s| s.text_color(theme::t().text))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(raw.clone()));
                })
                .child("复制"),
        );
    }
    let mut card = div()
        .relative()
        .ml_1()
        .mt_1()
        .mb_1()
        .overflow_hidden()
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .child(banner);
    // 状态点：卡片自己的 gutter 列（左 8px），对首行行盒垂直居中
    card = card.child(
        div()
            .absolute()
            .left(px(8.0))
            .top(px(14.0))
            .child(state_dot(dot_color)),
    );
    if !running {
        card = card.child(div().h(px(1.0)).w_full().bg(theme::t().border_l2));
        let empty = output.map(|o| o.trim().is_empty()).unwrap_or(true);
        if empty {
            card = card.child(
                div()
                    .pt(px(12.0))
                    .pr(px(14.0))
                    .pb(px(12.0))
                    .pl(px(30.0))
                    .font_family(theme_mono())
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(theme::t().text_3)
                    .child("无输出"),
            );
        } else {
            let text = output.unwrap_or_default();
            let mut out = div()
                .id(("term-out", uid))
                .pt(px(12.0))
                .pr(px(14.0))
                .pb(px(12.0))
                .pl(px(30.0))
                .max_h(px(224.0))
                .overflow_y_scroll()
                .overflow_x_scroll()
                .font_family(theme_mono())
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme::t().text);
            // 尾部换行是终结符，不是额外空行（web parse 后按渲染行裁剪）
            let trimmed = text.strip_suffix('\n').unwrap_or(text);
            for line in trimmed.split('\n') {
                out = out.child(div().min_h(px(18.0)).whitespace_nowrap().child(line.to_string()));
            }
            card = card.child(out);
        }
    }
    card
}

/// 终端 prompt 的 cwd 标签（web promptLabel：home 折叠 ~，否则末段）。
fn prompt_label(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if let Ok(home) = std::env::var("HOME")
        && trimmed == home.trim_end_matches(['/', '\\'])
    {
        return "~".into();
    }
    match trimmed.rsplit(['/', '\\']).next() {
        Some(seg) if !seg.is_empty() => seg.to_string(),
        _ => cwd.to_string(),
    }
}

/// 读取卡（web ReadBlock：banner（banner 底、路径 mono 标签 + 语言 + 复制）
/// + 48px 行号 gutter 正文，mono 13/22，8 行上限折叠）。
pub(crate) fn read_card(
    uid: u64,
    label: &str,
    lang: &str,
    text: &str,
    expanded: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + Clone,
) -> Div {
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').collect();
    let total = lines.len();
    let hidden = total.saturating_sub(CARD_MAX_LINES);
    let capped = hidden > 0 && !expanded;
    let (head, tail) = if capped {
        (CARD_MAX_LINES - CARD_MAX_LINES / 2, CARD_MAX_LINES / 2)
    } else {
        (total, 0)
    };
    let mut body = div()
        .id(("read-body", uid))
        .py(px(12.0))
        .overflow_x_scroll()
        .font_family(theme_mono())
        .text_size(px(13.0))
        .line_height(px(22.0));
    let row = |num: usize, text: &str| -> Div {
        div()
            .flex()
            .min_h(px(22.0))
            .whitespace_nowrap()
            .child(
                div()
                    .flex_none()
                    .w(px(48.0))
                    .pr(px(14.0))
                    .text_right()
                    .text_color(theme::t().text_3)
                    .child(num.to_string()),
            )
            .child(div().text_color(theme::t().text).child(text.to_string()))
    };
    for (i, line) in lines[..head].iter().enumerate() {
        body = body.child(row(i + 1, line));
    }
    if hidden > 0 {
        body = body.child(
            fold_toggle(uid, hidden, expanded, on_toggle.clone()).pl(px(48.0)),
        );
    }
    if tail > 0 {
        for (i, line) in lines[total - tail..].iter().enumerate() {
            body = body.child(row(total - tail + i + 1, line));
        }
    }
    let mut banner = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px(px(14.0))
        .py(px(9.0))
        .bg(theme::t().code_banner)
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(theme_mono())
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme::t().text)
                .child(label.to_string()),
        );
    let mut action = div().flex_none().flex().items_center().gap_3();
    if !lang.is_empty() {
        action = action.child(
            div()
                .font_family(theme_mono())
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme::t().text_3)
                .child(lang.to_string()),
        );
    }
    let raw = text.to_string();
    action = action.child(
        div()
            .id(("read-copy", uid))
            .cursor_pointer()
            .text_size(px(13.0))
            .line_height(px(18.0))
            .text_color(theme::t().text_2)
            .hover(|s| s.text_color(theme::t().text))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(raw.clone()));
            })
            .child("复制"),
    );
    banner = banner.child(action);
    div()
        .ml_1()
        .mt_1()
        .mb_1()
        .overflow_hidden()
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .child(banner)
        .child(body)
}

/// 差异卡（web DiffBlock，fs write 单 hunk：oldText = null → 全 + 行，
/// 路径头 600 weight，复制钮悬浮右上，footer `└ +A -R · N 个文件`，
/// mono 13/22，8 行上限折叠）。
pub(crate) fn diff_card(
    uid: u64,
    path: &str,
    new_text: &str,
    expanded: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + Clone,
) -> Div {
    let lines: Vec<&str> = new_text.strip_suffix('\n').unwrap_or(new_text).split('\n').collect();
    let added = lines.len();
    let total = added + 1; // + 路径头
    let hidden = total.saturating_sub(CARD_MAX_LINES);
    let capped = hidden > 0 && !expanded;
    let (head, tail) = if capped {
        (CARD_MAX_LINES - CARD_MAX_LINES / 2, CARD_MAX_LINES / 2)
    } else {
        (total, 0)
    };
    let copy_rows = {
        let mut s = String::new();
        s.push_str(path);
        s.push('\n');
        for l in &lines {
            s.push_str("+ ");
            s.push_str(l);
            s.push('\n');
        }
        s
    };
    let mut body = div()
        .id(("diff-body", uid))
        .p(px(12.0))
        .overflow_x_scroll()
        .font_family(theme_mono())
        .text_size(px(13.0))
        .line_height(px(22.0));
    let path_row = || {
        div()
            .min_h(px(22.0))
            .whitespace_nowrap()
            .pr(px(56.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::t().text)
            .child(path.to_string())
    };
    let add_row = |text: &str| {
        div()
            .min_h(px(22.0))
            .whitespace_nowrap()
            .text_color(theme::t().green)
            .child(format!("+ {text}"))
    };
    // 行序列 = [路径头, +行…]；8 行上限切头尾（行按索引惰性构建，Div 不可克隆）
    let row_for = |i: usize| -> Div {
        if i == 0 {
            path_row()
        } else {
            add_row(lines[i - 1])
        }
    };
    for i in 0..head {
        body = body.child(row_for(i));
    }
    if hidden > 0 {
        body = body.child(fold_toggle(uid, hidden, expanded, on_toggle.clone()));
    }
    if tail > 0 {
        for i in (total - tail)..total {
            body = body.child(row_for(i));
        }
    }
    let mut card = div()
        .relative()
        .ml_1()
        .mt_1()
        .mb_1()
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .child(body)
        .child(
            div()
                .px(px(14.0))
                .pb(px(12.0))
                .font_family(theme_mono())
                .text_size(px(13.0))
                .line_height(px(22.0))
                .text_color(theme::t().text_3)
                .child(format!("└ +{added} -0 · 1 个文件")),
        );
    card = card.child(
        div()
            .id(("diff-copy", uid))
            .absolute()
            .top(px(8.0))
            .right(px(12.0))
            .cursor_pointer()
            .text_size(px(13.0))
            .line_height(px(18.0))
            .text_color(theme::t().text_2)
            .hover(|s| s.text_color(theme::t().text))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_rows.clone()));
            })
            .child("复制"),
    );
    card
}

/// 网页获取卡（web WebFetchBlock：URL 链接（business 蓝、mono 13/19、
/// break-all）+ 状态行；dsh-web 无 HTTP 状态码元数据，只画截断注记）。
pub(crate) fn web_fetch_card(uid: u64, url: &str, truncated: bool) -> Div {
    let open_url = url.to_string();
    div()
        .ml_1()
        .mt_1()
        .mb_1()
        .p(px(12.0))
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .v_flex()
        .gap(px(6.0))
        .child(
            div()
                .id(("web-url", uid))
                .cursor_pointer()
                .font_family(theme_mono())
                .text_size(px(13.0))
                .line_height(px(19.0))
                .text_color(theme::t().accent)
                .hover(|s| s.underline())
                .on_click(move |_, _, cx| {
                    cx.open_url(&open_url);
                })
                .child(url.to_string()),
        )
        .when(truncated, |card| {
            card.child(
                div()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .text_color(theme::t().text_3)
                    .child("内容已截断"),
            )
        })
}

/// grep 结果的结构化还原（web 经结果 presentationMeta 结构化传递；本实现
/// 从模型可见文本回解析——格式由 dsh-search 定义，确定性可解析）。
pub(crate) struct GrepSearch {
    pub truncated: bool,
    pub total: usize,
    pub groups: Vec<(String, Vec<(usize, String)>)>,
}
/// 解析 `Found N matches` / `Found K of N matches` 头 + `path` + `Line N:
/// text` 分组正文；`No matches found` → 空 groups。非 grep 文本返回 None。
pub(crate) fn parse_grep_result(text: &str) -> Option<GrepSearch> {
    let mut lines = text.lines();
    let header = lines.next()?;
    if header == "No matches found" {
        return Some(GrepSearch { truncated: false, total: 0, groups: Vec::new() });
    }
    let truncated = header.contains(" of ");
    let total: usize = if truncated {
        let after = header.split(" of ").nth(1)?;
        after.split(' ').next()?.parse().ok()?
    } else {
        let body = header.strip_prefix("Found ")?.strip_suffix(" matches")
            .or_else(|| header.strip_prefix("Found ")?.strip_suffix(" match"))?;
        body.parse().ok()?
    };
    let mut groups: Vec<(String, Vec<(usize, String)>)> = Vec::new();
    for section in lines.collect::<Vec<_>>().join("\n").split("\n\n") {
        let mut sec = section.lines();
        let path = sec.next()?.to_string();
        let mut matches: Vec<(usize, String)> = Vec::new();
        for row in sec {
            let rest = row.strip_prefix("Line ")?;
            let (num, text) = rest.split_once(": ")?;
            matches.push((num.parse().ok()?, text.to_string()));
        }
        groups.push((path, matches));
    }
    Some(GrepSearch { truncated, total, groups })
}

/// 文件组折叠回调（每次按组下标构造）。
/// glob 结果的结构化还原（web globSearchMeta paths 形态；截断信息来自
/// `… of N paths` 脚注）。
pub(crate) struct GlobPaths {
    pub truncated: bool,
    pub total: usize,
    pub paths: Vec<String>,
}

pub(crate) fn parse_glob_result(text: &str) -> Option<GlobPaths> {
    if text.trim() == "No files found" {
        return Some(GlobPaths { truncated: false, total: 0, paths: Vec::new() });
    }
    let mut paths = Vec::new();
    let mut truncated = false;
    let mut total = 0usize;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("(Showing ") {
            // "(Showing K of N paths. …)"
            truncated = true;
            total = rest.split(" of ").nth(1)?.split(' ').next()?.parse().ok()?;
            continue;
        }
        if !line.is_empty() {
            paths.push(line.to_string());
        }
    }
    if !truncated {
        total = paths.len();
    }
    Some(GlobPaths { truncated, total, paths })
}

/// 搜索卡数据（web SearchBlock 的两种 kind）。
pub(crate) enum SearchCardData<'a> {
    Matches { search: &'a GrepSearch },
    Paths { paths: &'a GlobPaths },
}

pub(crate) type GroupToggleFactory =
    Box<dyn Fn(usize) -> Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>;

/// 搜索卡（web SearchBlock 两种 kind：matches=分组匹配（文件头可折叠、
/// 尾片组头补还），paths=扁平路径列表；banner 底摘要头 + 复制，mono 13/22，
/// 8 行上限头 4 尾 4）。
pub(crate) fn search_card(
    uid: u64,
    data: SearchCardData<'_>,
    expanded: bool,
    collapsed_groups: &[usize],
    on_fold: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + Clone,
    on_group: GroupToggleFactory,
) -> Div {
    // 形态相关：摘要文案、复制文本、扁平行序
    enum SRow {
        File(usize),
        Match(usize, usize, String),
        Path(String),
    }
    let (summary, copy_text, rows, empty): (String, String, Vec<SRow>, bool) = match &data {
        SearchCardData::Matches { search } => {
            let groups = &search.groups;
            let shown: usize = groups.iter().map(|(_, m)| m.len()).sum();
            let summary = if search.truncated {
                format!("显示 {shown} / 共 {} 处匹配 · {} 个文件", search.total, groups.len())
            } else {
                format!("{shown} 处匹配 · {} 个文件", groups.len())
            };
            let mut copy = String::new();
            let mut rows = Vec::new();
            for (gi, (_, matches)) in groups.iter().enumerate() {
                let collapsed = collapsed_groups.contains(&gi);
                rows.push(SRow::File(gi));
                if collapsed {
                    continue;
                }
                for (n, line) in matches {
                    rows.push(SRow::Match(gi, *n, line.clone()));
                }
            }
            for (i, (path, matches)) in groups.iter().enumerate() {
                if i > 0 {
                    copy.push_str("\n\n");
                }
                copy.push_str(path);
                for (n, line) in matches {
                    copy.push_str(&format!("\n{n}: {line}"));
                }
            }
            let empty = rows.is_empty();
            (summary, copy, rows, empty)
        }
        SearchCardData::Paths { paths } => {
            let shown = paths.paths.len();
            let summary = if paths.truncated {
                format!("显示 {shown} / 共 {} 个路径", paths.total)
            } else {
                format!("{shown} 个路径")
            };
            let copy = paths.paths.join("\n");
            let rows = paths.paths.iter().map(|p| SRow::Path(p.clone())).collect();
            let empty = paths.paths.is_empty();
            (summary, copy, rows, empty)
        }
    };
    let shown = match &data {
        SearchCardData::Matches { search } => search.groups.iter().map(|(_, m)| m.len()).sum(),
        SearchCardData::Paths { paths } => paths.paths.len(),
    };
    let mut header = div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(14.0))
        .py(px(9.0))
        .bg(theme::t().code_banner)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(theme::t().text_2)
                .child(summary),
        );
    if shown > 0 {
        header = header.child(
            div()
                .id(("search-copy", uid))
                .flex_none()
                .cursor_pointer()
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(theme::t().text_2)
                .hover(|s| s.text_color(theme::t().text))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                })
                .child("复制"),
        );
    }
    let total_rows = rows.len();
    let hidden = total_rows.saturating_sub(CARD_MAX_LINES);
    let capped = hidden > 0 && !expanded;
    let (head, tail_range) = if capped {
        let h = CARD_MAX_LINES - CARD_MAX_LINES / 2;
        (h, total_rows - (CARD_MAX_LINES - h)..total_rows)
    } else {
        (total_rows, 0..0)
    };
    let groups_ref: Option<&Vec<(String, Vec<(usize, String)>)>> = match &data {
        SearchCardData::Matches { search } => Some(&search.groups),
        SearchCardData::Paths { .. } => None,
    };
    let render_row = |row: &SRow, on_group: &GroupToggleFactory| -> AnyElement {
        match row {
            SRow::File(gi) => {
                let Some(groups) = groups_ref else { return div().into_any_element() };
                let Some((path, matches)) = groups.get(*gi) else { return div().into_any_element() };
                let count = matches.len();
                div()
                    .id(SharedString::from(format!("search-g-{uid}-{gi}")))
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .px(px(14.0))
                    .min_h(px(22.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::t().hover))
                    .on_click(on_group(*gi))
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_nowrap()
                            .font_family(theme_mono())
                            .text_size(px(13.0))
                            .line_height(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::t().text)
                            .child(path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme_mono())
                            .text_size(px(13.0))
                            .line_height(px(22.0))
                            .text_color(theme::t().text_3)
                            .child(count.to_string()),
                    )
                    .into_any_element()
            }
            SRow::Match(_, n, line) => div()
                .flex()
                .items_baseline()
                .min_h(px(22.0))
                .whitespace_nowrap()
                .pl(px(14.0))
                .font_family(theme_mono())
                .text_size(px(13.0))
                .line_height(px(22.0))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::t().text_3)
                        .child(format!("{n}: ")),
                )
                .child(div().min_w_0().text_color(theme::t().text).child(line.clone()))
                .into_any_element(),
            SRow::Path(path) => div()
                .min_h(px(22.0))
                .whitespace_nowrap()
                .pl(px(14.0))
                .text_color(theme::t().text)
                .child(path.clone())
                .into_any_element(),
        }
    };
    let mut body = div()
        .id(("search-body", uid))
        .pt(px(8.0))
        .pr(px(14.0))
        .pb(px(12.0))
        .overflow_x_scroll()
        .font_family(theme_mono())
        .text_size(px(13.0))
        .line_height(px(22.0));
    let head_rows: Vec<&SRow> = rows[..head.min(rows.len())].iter().collect();
    for row in &head_rows {
        body = body.child(render_row(row, &on_group));
    }
    // 尾片首行是匹配行且其组头不在头片时：补还文件头行并消耗一个尾位
    // （web SearchBlock 的 tailHeader 语义），可见行数与 hidden 保持不变
    let mut tail_rows: Vec<&SRow> = rows[tail_range].iter().collect();
    let tail_header: Option<&SRow> = match tail_rows.first() {
        Some(SRow::Match(gi, ..)) if !head_rows.iter().any(|r| matches!(r, SRow::File(g2) if g2 == gi)) => {
            rows.iter().find(|r| matches!(r, SRow::File(g2) if g2 == gi))
        }
        _ => None,
    };
    if tail_header.is_some() {
        tail_rows.remove(0);
    }
    if hidden > 0 {
        body = body.child(fold_toggle(uid, hidden, expanded, on_fold).px(px(14.0)));
    }
    if let Some(header_row) = tail_header {
        body = body.child(render_row(header_row, &on_group));
    }
    for row in &tail_rows {
        body = body.child(render_row(row, &on_group));
    }
    div()
        .ml_1()
        .mt_1()
        .mb_1()
        .overflow_hidden()
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .child(header)
        .when(empty, |card| {
            card.child(
                div()
                    .px(px(14.0))
                    .py(px(12.0))
                    .font_family(theme_mono())
                    .text_size(px(13.0))
                    .line_height(px(22.0))
                    .text_color(theme::t().text_3)
                    .child("未找到结果"),
            )
        })
        .when(!empty, |card| card.child(body))
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
        .border_color(theme::t().border_l1)
        .bg(theme::t().code_bg);
    card = card.child(io_section(uid * 2, "输入", input, false));
    if let Some(out) = output {
        card = card.child(div().h(px(1.0)).w_full().bg(theme::t().border_l2));
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
                .text_color(theme::t().caption)
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
                .text_color(if error { theme::t().error } else { theme::t().text_2 })
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
                .text_color(theme::t().text_2)
                .child(label.to_string()),
        )
        .child(content)
}
/// 详情面板代码卡（web .code：r12、pad 16、mono 13/22）。
pub(crate) fn code_card(text: &str, error: bool) -> Div {
    div()
        .p_4()
        .rounded(px(12.0))
        .bg(theme::t().code_bg)
        .font_family(theme_mono())
        .text_size(px(13.0))
        .line_height(px(22.0))
        .text_color(if error { theme::t().error } else { theme::t().text })
        
        
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

/// 工具行折叠行模型（web toolRowModel 语义）：
/// 标题按工具/op 定名（tool.title.*），摘要取 args 的人类字段
/// （SUMMARY_KEYS：command / path / url），fs read|write 的 path 作为
/// 可打开文件链接返回（deriveFilePath）。
pub(crate) fn tool_row_texts(name: &str, args: &str) -> (String, String, Option<String>) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(args).ok();
    let pick = |keys: &[&str]| -> Option<String> {
        let v = parsed.as_ref()?;
        keys.iter()
            .find_map(|k| v.get(k).and_then(|x| x.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().next().unwrap_or("").to_string())
    };
    match name {
        "shell" => (
            "Bash".into(),
            pick(&["command"]).unwrap_or_else(|| first_line(args)),
            None,
        ),
        "web_fetch" => (
            "网页获取".into(),
            pick(&["url"]).unwrap_or_else(|| first_line(args)),
            None,
        ),
        "grep" => (
            "搜索".into(),
            pick(&["pattern"]).unwrap_or_else(|| first_line(args)),
            None,
        ),
        "glob" => (
            "搜索".into(),
            pick(&["pattern"]).unwrap_or_else(|| first_line(args)),
            None,
        ),
        "subagent" => (
            "子任务".into(),
            pick(&["description"]).unwrap_or_else(|| first_line(args)),
            None,
        ),
        "fs" => {
            let op = parsed
                .as_ref()
                .and_then(|v| v.get("op"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let path = pick(&["path"]);
            let title = if op == "write" { "写入" } else { "读取" };
            let summary = path.clone().unwrap_or_else(|| first_line(args));
            let link = match op {
                "read" | "write" => path,
                _ => None,
            };
            (title.into(), summary, link)
        }
        other => (
            "工具调用".into(),
            format!("{other} · {}", first_line(args)),
            None,
        ),
    }
}

/// 摘要路径显示（web relativizeToCwd + abbreviateHomePath）：
/// 先剥工作区根，剩余主目录绝对路径缩写为 ~。
pub(crate) fn display_path(text: &str, cwd: &str) -> String {
    let root = cwd.trim_end_matches(['/', '\\']);
    let text = if !root.is_empty()
        && (text.starts_with(&format!("{root}/")) || text.starts_with(&format!("{root}\\")))
    {
        text[root.len() + 1..].to_string()
    } else {
        text.to_string()
    };
    if let Ok(home) = std::env::var("HOME") {
        let home = home.trim_end_matches('/');
        if !home.is_empty() && text.starts_with(&format!("{home}/")) {
            return format!("~{}", &text[home.len()..]);
        }
    }
    text
}

/// 用宿主默认应用打开文件（web onOpenFile 语义）。
pub(crate) fn open_with_host_app(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", path])
        .spawn();
}

/// 多行文本取首行（截断 120 字符）。
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    line.chars().take(120).collect()
}

/// 当前目录名（hero 已换「选择工作区」chip，保留备用）。
#[allow(dead_code)]
pub(crate) fn workspace_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "workspace".into())
}

pub(crate) fn theme_mono() -> SharedString {
    "Consolas".into()
}
