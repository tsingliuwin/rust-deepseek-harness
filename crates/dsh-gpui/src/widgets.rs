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
        .when(!time_label.is_empty(), |d| {
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
        .child(
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
                .on_click(on_more)
                .child(Icon::new(IconName::Ellipsis).size(px(14.0)).text_color(theme::t().text_3)),
        )
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
