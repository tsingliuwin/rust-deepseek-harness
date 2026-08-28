//! 设置面板 —— 1:1 对齐 web `ui-settings-general/SettingsRoot`：
//! 遮罩 + 800px r24 面板（188px 左导航 + 54px 头 + 滚动内容区）。
//! 页：通用设置（外观/语言/Enter 行为）、模型（DeepSeek key + 模型）、
//! 插件 / Agent 预设（空态占位，agent 层尚未接入这两个系统）。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt, input::Input};

use crate::{AppView, AppearanceMode, EnterBehavior, SettingsTab};
use crate::theme;

/// 设置弹层（挂在根视图最上层）。
pub(crate) fn render_settings(app: &AppView, this: Entity<AppView>, window: &mut Window, cx: &mut App) -> Div {
    let tk = theme::t();
    let t_close = this.clone();

    // 导航（web .nav：188px、pad(22,12,0)、gap 18）
    let nav = div()
        .w(px(188.0))
        .flex_none()
        .v_flex()
        .pt(px(22.0))
        .px_3()
        .gap(px(18.0))
        .child(
            div()
                .px_3()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child("设置"),
        )
        .child(
            div()
                .v_flex()
                .gap_1()
                .child(nav_cell("set-nav-general", IconName::Settings, "通用设置", app.settings_tab == SettingsTab::General, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::General; cx.notify(); })
                }))
                .child(nav_cell("set-nav-models", IconName::Bot, "模型", app.settings_tab == SettingsTab::Models, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Models; cx.notify(); })
                }))
                .child(nav_cell("set-nav-plugins", IconName::Palette, "插件", app.settings_tab == SettingsTab::Plugins, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Plugins; cx.notify(); })
                }))
                .child(nav_cell("set-nav-presets", IconName::SquareTerminal, "Agent 预设", app.settings_tab == SettingsTab::Presets, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Presets; cx.notify(); })
                })),
        );

    // 头（web .header：54px、pad(20,14,8,10)、右侧动作）
    let t_open = this.clone();
    let header = div()
        .flex_none()
        .h(px(54.0))
        .flex()
        .items_center()
        .pt_5()
        .pb_2()
        .pl(px(10.0))
        .pr(px(14.0))
        .child(div().flex_1())
        .child(
            div()
                .id("set-open-config")
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(14.0))
                .text_size(px(theme::FONT_TAB))
                .line_height(px(20.0))
                .text_color(tk.text_2)
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, _cx| {
                    // 打开配置目录（settings.json / sessions 所在）
                    let dir = crate::config_dir();
                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    let _ = &t_open;
                })
                .child("打开配置文件"),
        )
        .child(
            div()
                .id("set-close")
                .size(px(28.0))
                .ml_2()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_color(tk.text)
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, cx| {
                    t_close.update(cx, |v, cx| { v.settings_open = false; cx.notify(); });
                })
                .child(Icon::new(IconName::Close).size(px(14.0))),
        );

    // 内容区（web .options：pad(0,24,24,24)、滚动）
    let content = match app.settings_tab {
        SettingsTab::General => general_page(app, &this),
        SettingsTab::Models => models_page(app, &this, window, cx),
        SettingsTab::Plugins => placeholder_page("插件", "插件系统尚未在 Rust 版接入；接入后将在此管理。"),
        SettingsTab::Presets => placeholder_page("Agent 预设", "预设（标准/计划/只读等模式）尚未在 Rust 版接入；接入后将在此选择。"),
    };
    let options = div()
        .id("set-options")
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .pr_6()
        .pl_6()
        .pb_6()
        .child(content);

    div()
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .bg(tk.mask)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(800.0))
                .h(px(700.0))
                .flex()
                .rounded(px(24.0))
                .overflow_hidden()
                .bg(tk.surface)
                .shadow_lg()
                .child(nav)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .v_flex()
                        .child(header)
                        .child(options),
                ),
        )
}

/// 导航格（web .navCell：40px、r12、pad(9,16,9,12)、14/22、active 填充）。
fn nav_cell(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(40.0))
        .flex()
        .items_center()
        .gap_2()
        .pl_3()
        .pr_4()
        .rounded(px(12.0))
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text)
        .map(|d| if active { d.bg(tk.elevated) } else { d })
        .when(!active, |d| d.hover(|s| s.bg(tk.hover)))
        .cursor_pointer()
        .on_click(on_click)
        .child(Icon::new(icon).size(px(16.0)).text_color(if active { tk.accent } else { tk.text_2 }))
        .child(label)
}

/// 设置行（web 通用页行：label + 说明 + 右控件，行间 l2 发丝线）。
fn settings_row(label: &str, desc: &str, control: Div, last: bool) -> Div {
    let tk = theme::t();
    let mut row = div()
        .w_full()
        .flex()
        .items_center()
        .gap_4()
        .pt_4()
        .pb_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .v_flex()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .text_color(tk.text)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .child(desc.to_string()),
                ),
        )
        .child(control);
    if !last {
        row = row.border_b_1().border_color(tk.border_l2);
    }
    row
}

/// 分段控件（h28 r14 胶囊；选中段 elevated 底 + primary 字）。
fn segmented(
    id: &'static str,
    options: &[(&'static str, bool, bool)], // (label, selected, enabled)
    on_pick: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static + Clone,
) -> Div {
    let tk = theme::t();
    let mut seg = div()
        .h(px(28.0))
        .flex()
        .items_center()
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(14.0))
        .bg(tk.layer1);
    for (i, (label, selected, enabled)) in options.iter().enumerate() {
        let on = on_pick.clone();
        let cell = div()
            .id(SharedString::from(format!("{id}-{i}")))
            .h(px(24.0))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(12.0))
            .text_size(px(theme::FONT_TAB))
            .line_height(px(20.0))
            .text_color(if *selected { tk.text } else { tk.text_3 })
            .map(|d| if *selected { d.bg(tk.elevated) } else { d })
            .when(*enabled, |d| {
                d.cursor_pointer()
                    .when(!selected, |d| d.hover(|s| s.bg(tk.hover)))
                    .on_click(move |e, w, cx| on(i, e, w, cx))
            })
            .when(!*enabled, |d| d.opacity(0.5))
            .child(*label);
        seg = seg.child(cell);
    }
    seg
}

/// 通用设置页。
fn general_page(app: &AppView, this: &Entity<AppView>) -> Div {
    let tk = theme::t();
    let t = this.clone();
    let appearance = segmented(
        "set-appearance",
        &[
            ("浅色", app.settings.appearance == AppearanceMode::Light, true),
            ("深色", app.settings.appearance == AppearanceMode::Dark, true),
            ("跟随系统", app.settings.appearance == AppearanceMode::System, true),
        ],
        move |i, _, _, cx| {
            let mode = match i {
                0 => AppearanceMode::Light,
                1 => AppearanceMode::Dark,
                _ => AppearanceMode::System,
            };
            t.update(cx, |v, cx| v.set_appearance(mode, cx));
        },
    );

    let t2 = this.clone();
    let language = segmented(
        "set-language",
        &[
            ("中文", true, true),
            ("English", false, false),
        ],
        move |_, _, _, _cx| {
            let _ = &t2;
        },
    );

    let t3 = this.clone();
    let enter = segmented(
        "set-enter",
        &[
            ("排队发送", app.settings.enter == EnterBehavior::Queue, true),
            ("打断", app.settings.enter == EnterBehavior::Interrupt, true),
        ],
        move |i, _, _, cx| {
            let behavior = if i == 0 { EnterBehavior::Queue } else { EnterBehavior::Interrupt };
            t3.update(cx, |v, cx| v.set_enter_behavior(behavior, cx));
        },
    );

    div()
        .v_flex()
        .pt_2()
        .child(
            div()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child("通用设置"),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.text_3)
                .child("设置保存在本地配置文件中，立即生效。"),
        )
        .child(settings_row("外观", "选择应用的外观主题", appearance, false))
        .child(settings_row("语言", "界面语言", language, false))
        .child(settings_row(
            "繁忙时 Enter 键行为",
            "仅在智能体运行时生效；Shift+Enter 始终换行",
            enter,
            true,
        ))
}

/// 模型页（web ModelsSection：标题 + 说明 + provider 卡）。
fn models_page(app: &AppView, this: &Entity<AppView>, _window: &mut Window, _cx: &mut App) -> Div {
    let tk = theme::t();
    let t_save = this.clone();
    let t_model = this.clone();

    let model_seg = segmented(
        "set-model",
        &[
            ("deepseek-chat", app.desired_model == "deepseek-chat", true),
            ("deepseek-reasoner", app.desired_model == "deepseek-reasoner", true),
        ],
        move |i, _, _, cx| {
            let model = if i == 0 { "deepseek-chat" } else { "deepseek-reasoner" };
            t_model.update(cx, |v, cx| {
                v.desired_model = model.to_string();
                v.agent.set_provider_and_model("deepseek", model.to_string());
                v.persist_settings();
                cx.notify();
            });
        },
    );

    div()
        .v_flex()
        .pt_2()
        .gap_3()
        .child(
            div()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child("模型"),
        )
        .child(
            div()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .text_color(tk.text_3)
                .child("配置模型提供方；保存后对新会话生效。"),
        )
        .child(
            // provider 卡（web .rowCard：l2 描边、r12、pad 12/14）
            div()
                .v_flex()
                .gap_3()
                .rounded(px(12.0))
                .border_1()
                .border_color(tk.border_l2)
                .px(px(14.0))
                .py_3()
                .child(
                    // 卡头：图标 + 名称 + 状态
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .child(Icon::new(IconName::Bot).size(px(16.0)).text_color(tk.accent))
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(22.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(tk.text)
                                .child("DeepSeek"),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(theme::FONT_CAPTION_LEADING))
                                .text_color(if app.llm_configured { tk.green } else { tk.text_3 })
                                .child(if app.llm_configured { "已连接" } else { "未配置（使用 mock）" }),
                        ),
                )
                .child(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(theme::FONT_CAPTION_LEADING))
                                .text_color(tk.text_3)
                                .child("API Key"),
                        )
                        .child(Input::new(&app.api_input).appearance(false).w_full())
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("set-save-key")
                                .h(px(28.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(px(14.0))
                                .bg(tk.accent)
                                .text_color(gpui::white())
                                .text_size(px(theme::FONT_TAB))
                                .line_height(px(20.0))
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .hover(|s| s.bg(tk.accent_hover))
                                .on_click(move |_, _, cx| {
                                    t_save.update(cx, |v, cx| {
                                        v.apply_api_key(cx);
                                        cx.notify();
                                    });
                                })
                                .child("保存并启用"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(theme::FONT_CAPTION_LEADING))
                                .text_color(tk.text_3)
                                .child("留空则保持当前路由。"),
                        ),
                )
                .child(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(theme::FONT_CAPTION_LEADING))
                                .text_color(tk.text_3)
                                .child("模型"),
                        )
                        .child(model_seg),
                ),
        )
}

/// 空态页（插件 / Agent 预设）。
fn placeholder_page(title: &str, desc: &str) -> Div {
    let tk = theme::t();
    div()
        .v_flex()
        .pt_2()
        .gap_2()
        .child(
            div()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .text_color(tk.text_3)
                .child(desc.to_string()),
        )
}
