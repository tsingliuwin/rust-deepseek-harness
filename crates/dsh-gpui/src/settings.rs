//! 设置面板 —— 1:1 对齐 web `ui-settings-general/SettingsRoot`：
//! 遮罩 + 800px r24 面板（188px 左导航 + 54px 头 + 滚动内容区）。
//! 页：通用设置（外观/语言/Enter 行为）、模型（DeepSeek key + 模型）、
//! 插件 / Agent 预设（空态占位，agent 层尚未接入这两个系统）。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt, input::{Input, InputState}};

use crate::{AppView, AppearanceMode, AddingMode, EnterBehavior, SettingsTab, PROVIDER_CATALOG, valid_route_id};
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

/// 模型页（web ModelsSection）：标题/说明 + 已配置提供方 rowCard 列表 +
/// addBlock（两枚虚线按钮 / adopt 卡 / declare 卡）。
fn models_page(app: &AppView, this: &Entity<AppView>, _window: &mut Window, cx: &mut App) -> Div {

    // --- 已配置提供方：DeepSeek rowCard + 自定义 rowCards ---
    let deepseek_row = provider_row(
        app,
        this,
        "deepseek",
        "DeepSeek",
        false, // 自定义 tag
        app.llm_configured,
        Some(()), // 可编辑，不可删除（内置路由）
    );

    let custom_rows: Vec<Div> = app
        .settings
        .providers
        .iter()
        .map(|p| {
            provider_row(
                app,
                this,
                &p.id,
                if p.name.is_empty() { &p.id } else { &p.name },
                true,
                !p.api_key.is_empty(),
                None, // 可编辑 + 可删除
            )
        })
        .collect();

    // --- addBlock ---
    let add_block = match app.adding {
        AddingMode::Adopt => adopt_card(app, this, cx),
        AddingMode::Declare => declare_card(app, this, cx),
        AddingMode::None => {
            let t1 = this.clone();
            let t2 = this.clone();
            div()
                .flex()
                .flex_wrap()
                .gap(px(10.0))
                .child(add_button("set-add-known", "添加提供方", move |_, _, cx| {
                    t1.update(cx, |v, cx| {
                        v.adding = AddingMode::Adopt;
                        cx.notify();
                    });
                }))
                .child(add_button("set-add-custom", "添加自定义提供方", move |_, _, cx| {
                    t2.update(cx, |v, cx| {
                        v.adding = AddingMode::Declare;
                        cx.notify();
                    });
                }))
        }
    };

    div()
        .v_flex()
        .pt_2()
        .gap_3()
        .child(page_title("模型"))
        .child(page_intro("填入各提供方的 API 密钥即可使用其模型。"))
        .child(deepseek_row)
        .children(custom_rows)
        .child(add_block)
}

/// 页标题 / 说明（web .title 16/24 wt500、.intro 14/22 tertiary）。
fn page_title(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(16.0))
        .line_height(px(24.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(tk.text)
        .child(text.to_string())
}

fn page_intro(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text_3)
        .child(text.to_string())
}

/// 已配置提供方行卡（web .rowCard：l2 描边 r12 pad 12/14 gap 12）。
/// `deletable`=None 表示内置路由（DeepSeek）只可编辑。
#[allow(clippy::too_many_arguments)]
fn provider_row(
    app: &AppView,
    this: &Entity<AppView>,
    id: &str,
    name: &str,
    custom: bool,
    has_key: bool,
    _builtin: Option<()>,
) -> Div {
    let tk = theme::t();
    let is_deepseek = id == "deepseek";
    let active = app.active_provider == id;
    let editing = app.editing_provider.as_deref() == Some(id);

    // 行头：凭据点 + 名称 + route 标注 + 自定义 tag + 操作
    let t_edit = this.clone();
    let edit_id = id.to_string();
    let mut head = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            // 凭据点（web .credentialDot：8px 圆，绿=已配置 红=缺失）
            div()
                .size(px(8.0))
                .rounded_full()
                .flex_none()
                .bg(if has_key { tk.green } else { tk.error }),
        )
        .child(
            div()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child(name.to_string()),
        );
    if name != id {
        head = head.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.text_3)
                .child(id.to_string()),
        );
    }
    if custom {
        head = head.child(
            // 「自定义」tag（web .rowTag：11/16、l3 描边、r4）
            div()
                .px(px(6.0))
                .py(px(1.0))
                .border_1()
                .border_color(tk.border_l3)
                .rounded(px(4.0))
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(tk.text_2)
                .child("自定义"),
        );
    }
    let mut head = head.child(div().flex_1());
    if active {
        head = head.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.green)
                .child("使用中"),
        );
    } else {
        let t_act = this.clone();
        let act_id = id.to_string();
        head = head.child(
            link_button(
                SharedString::from(format!("prov-use-{id}")),
                "启用",
                move |_, _, cx| {
                    let id = act_id.clone();
                    t_act.update(cx, |v, cx| {
                        v.activate_provider(&id);
                        cx.notify();
                    });
                },
            ),
        );
    }
    head = head.child(
        // 编辑（secondary h36 r18）
        action_button(
            SharedString::from(format!("prov-edit-{id}")),
            "编辑",
            false,
            move |_, _, cx| {
                let id = edit_id.clone();
                t_edit.update(cx, |v, cx| {
                    v.editing_provider = if v.editing_provider.as_deref() == Some(&id) {
                        None
                    } else {
                        Some(id)
                    };
                    cx.notify();
                });
            },
        ),
    );
    if !is_deepseek {
        let t_del = this.clone();
        let del_id = id.to_string();
        head = head.child(
            action_button(
                SharedString::from(format!("prov-del-{id}")),
                "删除",
                false,
                move |_, _, cx| {
                    let id = del_id.clone();
                    t_del.update(cx, |v, cx| {
                        v.remove_provider(&id);
                        cx.notify();
                    });
                },
            ),
        );
    }

    let mut card = div()
        .v_flex()
        .gap_3()
        .rounded(px(12.0))
        .border_1()
        .border_color(tk.border_l2)
        .px(px(14.0))
        .py_3()
        .child(head);

    // 展开的编辑卡（web rowCard 内嵌 ProviderEditor）
    if editing {
        card = card.child(edit_card(app, this, id, name));
    }
    card
}

/// 编辑卡（web ProviderEditor：密钥 + 自定义设置折叠 + 保存/取消）。
fn edit_card(app: &AppView, this: &Entity<AppView>, id: &str, name: &str) -> Div {
    let tk = theme::t();
    let t_save = this.clone();
    let models: Vec<String> = if id == "deepseek" {
        vec!["deepseek-chat".into(), "deepseek-reasoner".into()]
    } else {
        app.settings
            .providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.models.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    };

    div()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        // editorHeader：标题 + route 标注
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .child(id.to_string()),
                ),
        )
        .child(field("API 密钥", Input::new(&app.edit_key).w_full()))
        .child(hint(if id == "deepseek" {
            "编辑卡默认 API 地址 https://api.deepseek.com；自定义提供方的 API 地址在折叠区内。"
        } else {
            "选择模型目录中的首行作为路由默认模型。"
        }))
        .child(
            // 自定义设置折叠（web details.customized：12/18 wt500 secondary + 旋转 chevron）
            disclosure(
                "edit-customized",
                "自定义设置",
                app.edit_customized_open,
                div()
                    .v_flex()
                    .gap(px(12.0))
                    .pt_3()
                    .child(field("API 地址", Input::new(&app.edit_base).w_full()))
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .line_height(px(theme::FONT_CAPTION_LEADING))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tk.text_2)
                                    .child("模型目录"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .line_height(px(theme::FONT_CAPTION_LEADING))
                                    .text_color(tk.text_3)
                                    .child(models.join(" · ")),
                            ),
                    )
                    .child(hint("其余字段在 settings.json 中，请直接编辑对应段。")),
                {
                    let t = this.clone();
                    move |cx: &mut App| {
                        t.update(cx, |v, cx| {
                            v.edit_customized_open = !v.edit_customized_open;
                            cx.notify();
                        });
                    }
                },
            ),
        )
        .child(
            // editorActions：取消 + 保存
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child({
                    let t = this.clone();
                    action_button("edit-cancel", "取消", false, move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            v.editing_provider = None;
                            cx.notify();
                        });
                    })
                })
                .child(action_button("edit-save", "保存", true, move |_, _, cx| {
                    t_save.update(cx, |v, cx| {
                        v.save_edit(cx);
                        cx.notify();
                    });
                })),
        )
}

/// 「添加提供方」卡（web addCard = 提供方选择 + ProviderEditor hideTitle）。
fn adopt_card(app: &AppView, this: &Entity<AppView>, _cx: &App) -> Div {
    let tk = theme::t();
    let t_save = this.clone();
    let t_cancel = this.clone();

    // 目录里未被 adopt 的提供方（DeepSeek 已内置）
    let candidates: Vec<usize> = (0..PROVIDER_CATALOG.len())
        .filter(|i| !app.settings.providers.iter().any(|p| p.id == PROVIDER_CATALOG[*i].id))
        .collect();
    let pick = candidates.iter().position(|i| *i == app.adopt_pick).map(|p| candidates[p]).unwrap_or(0);
    let entry = &PROVIDER_CATALOG[pick];

    let mut chooser = div().flex().flex_wrap().gap_2();
    for (slot, ci) in candidates.iter().enumerate() {
        let selected = *ci == pick;
        let t = this.clone();
        let idx = *ci;
        let e = &PROVIDER_CATALOG[idx];
        chooser = chooser.child(
            div()
                .id(SharedString::from(format!("adopt-pick-{idx}")))
                .h(px(32.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(8.0))
                .border_1()
                .border_color(if selected { tk.accent.into() } else { tk.border_l2 })
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .text_color(if selected { tk.accent } else { tk.text })
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| {
                        v.adopt_pick = idx;
                        cx.notify();
                    });
                })
                .child(e.name),
        );
        let _ = slot;
    }

    div()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        .child(field("提供方", chooser))
        .child(field("API 密钥", Input::new(&app.adopt_key).w_full()))
        .child(
            disclosure(
                "adopt-customized",
                "自定义设置",
                app.adopt_customized_open,
                div()
                    .v_flex()
                    .gap(px(12.0))
                    .pt_3()
                    .child(field("API 地址", Input::new(&app.adopt_base).w_full()))
                    .child(
                        div()
                            .v_flex()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .line_height(px(theme::FONT_CAPTION_LEADING))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tk.text_2)
                                    .child("模型目录"),
                            )
                            .child(hint(&format!("正在使用适配器默认模型（{}）", entry.model))),
                    ),
                {
                    let t = this.clone();
                    move |cx: &mut App| {
                        t.update(cx, |v, cx| {
                            v.adopt_customized_open = !v.adopt_customized_open;
                            cx.notify();
                        });
                    }
                },
            ),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(action_button("adopt-cancel", "取消", false, move |_, _, cx| {
                    t_cancel.update(cx, |v, cx| {
                        v.adding = AddingMode::None;
                        cx.notify();
                    });
                }))
                .child(action_button("adopt-save", "保存", true, move |_, window, cx| {
                    let (key, base) = t_save.read_with(cx, |v, _| (v.adopt_key.clone(), v.adopt_base.clone()));
                    let outcome = t_save.update(cx, |v, cx| {
                        let r = v.adopt_provider(cx);
                        if r.is_ok() {
                            v.adding = AddingMode::None;
                        }
                        cx.notify();
                        r
                    });
                    if outcome.is_ok() {
                        key.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                        base.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                    }
                })),
        )
}

/// 「添加自定义提供方」卡（web CustomProviderCard：六字段 + 模型目录 + 创建）。
fn declare_card(app: &AppView, this: &Entity<AppView>, cx: &App) -> Div {
    let tk = theme::t();
    let t_create = this.clone();
    let t_cancel = this.clone();
    let t_add_model = this.clone();

    // route 校验提示（web：invalid/taken → error；否则 hint）
    let route_val = app.dc_route.read_with(cx, |s, _| s.value().trim().to_string());
    let route_msg = if !route_val.is_empty() && !valid_route_id(&route_val) {
        ("需以小写字母开头，之后可用小写字母、数字和短横线。", true)
    } else if app.settings.providers.iter().any(|p| p.id == route_val) || route_val == "deepseek" {
        ("已有提供方使用了这个 ID。", true)
    } else {
        ("以小写字母开头的标识，在请求中唯一标识该提供方，并用于派生凭据名。", false)
    };

    // 模型行列表（web .modelList/.modelEntry：l2 描边 r8 pad6 + 删除）
    let mut model_list = div().v_flex().gap_2();
    for (i, m) in app.dc_models.iter().enumerate() {
        let t = this.clone();
        model_list = model_list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(8.0))
                .border_1()
                .border_color(tk.border_l2)
                .px(px(10.0))
                .py(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .text_color(tk.text)
                        .child(m.clone()),
                )
                .child({
                    let t = t.clone();
                    div()
                        .id(SharedString::from(format!("dc-model-del-{i}")))
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .cursor_pointer()
                        .hover(|s| s.text_color(tk.text_2).bg(tk.hover))
                        .on_click(move |_, _, cx| {
                            let i = i;
                            t.update(cx, |v, cx| {
                                v.dc_models.remove(i);
                                cx.notify();
                            });
                        })
                        .child("删除模型")
                }),
        );
    }

    let needs_hint = if app.dc_models.is_empty() {
        Some("自定义提供方至少需要一个模型。")
    } else {
        None
    };

    div()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        // editorHeader
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text)
                        .child("自定义提供方"),
                ),
        )
        .child(field("Provider ID", Input::new(&app.dc_route).w_full()))
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(if route_msg.1 { tk.error } else { tk.text_3 })
                .child(route_msg.0),
        )
        .child(field("显示名称", Input::new(&app.dc_name).w_full()))
        .child(field("API 地址", Input::new(&app.dc_base).w_full()))
        .child(field("API 协议", protocol_field("openai")))
        .child(field("API 密钥", Input::new(&app.dc_key).w_full()))
        // 模型目录
        .child(
            div()
                .v_flex()
                .gap(px(10.0))
                .pt_3()
                .border_t_1()
                .border_color(tk.border_l2)
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text_2)
                        .child("模型目录"),
                )
                .child(model_list)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Input::new(&app.dc_new_model).w(px(240.0)))
                        .child(link_button("dc-add-model", "添加模型", move |_, window, cx| {
                            let entities = t_add_model.read_with(cx, |v, _| v.dc_new_model.clone());
                            t_add_model.update(cx, |v, cx| {
                                v.dc_push_model(cx);
                                cx.notify();
                            });
                            entities.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                        })),
                )
                .children(needs_hint.map(hint)),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(action_button("declare-cancel", "取消", false, move |_, window, cx| {
                    let ents = t_cancel.read_with(cx, |v, _| {
                        (v.dc_route.clone(), v.dc_name.clone(), v.dc_base.clone(), v.dc_key.clone(), v.dc_new_model.clone())
                    });
                    t_cancel.update(cx, |v, cx| {
                        v.adding = AddingMode::None;
                        v.dc_models.clear();
                        cx.notify();
                    });
                    let (a, b, c, d, e) = ents;
                    for ent in [a, b, c, d, e] {
                        ent.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                    }
                }))
                .child(action_button("declare-create", "创建提供方", true, move |_, window, cx| {
                    let outcome = t_create.update(cx, |v, cx| {
                        let r = v.declare_provider(cx);
                        if r.is_ok() {
                            v.adding = AddingMode::None;
                            v.dc_models.clear();
                        }
                        cx.notify();
                        r
                    });
                    if outcome.is_ok() {
                        let ents = t_create.read_with(cx, |v, _| {
                            (v.dc_route.clone(), v.dc_name.clone(), v.dc_base.clone(), v.dc_key.clone(), v.dc_new_model.clone())
                        });
                        let (a, b, c, d, e) = ents;
                        for ent in [a, b, c, d, e] {
                            ent.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                        }
                    }
                })),
        )
}

/// 字段行（web .field：label 12/18 wt500 secondary + 控件 gap6）。
fn field(label: &str, control: impl IntoElement) -> Div {
    let tk = theme::t();
    div()
        .v_flex()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text_2)
                .child(label.to_string()),
        )
        .child(control)
}

/// 12/18 tertiary 提示行（web .advancedHint）。
fn hint(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(tk.text_3)
        .child(text.to_string())
}

/// 协议字段（web select：右侧 12px chevron 的胶囊框，恒 openai）。
fn protocol_field(value: &'static str) -> Div {
    let tk = theme::t();
    div()
        .w(px(240.0))
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_between()
        .px(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(tk.border_l2)
        .bg(tk.layer1)
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text)
        .child(value)
        .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(tk.caption))
}

/// 折叠区（web details.customized：summary 12/18 wt500 secondary + 旋转 chevron）。
fn disclosure(id: &'static str, label: &'static str, open: bool, body: Div, on_toggle: impl Fn(&mut App) + 'static) -> Div {
    let tk = theme::t();
    let mut col = div().v_flex().child(
        div()
            .id(id)
                        .flex()
            .items_center()
            .gap(px(6.0))
            .px_1()
            .text_size(px(theme::FONT_CAPTION))
            .line_height(px(theme::FONT_CAPTION_LEADING))
            .font_weight(FontWeight::MEDIUM)
            .text_color(tk.text_2)
            .cursor_pointer()
            .hover(|s| s.text_color(tk.text))
            .on_click(move |_, _, cx| on_toggle(cx))
            .child(if open { "▾" } else { "▸" })
            .child(label),
    );
    if open {
        col = col.child(body);
    }
    col
}

/// h36 r18 动作按钮（web .primaryButton/.secondaryButton）。
/// primary = 品牌填充（暗色主题为浅底深字的反转设计）。
fn action_button(
    id: impl Into<ElementId>,
    label: &'static str,
    primary: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(36.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(18.0))
        .map(|d| {
            if primary {
                // web button-primary：fill=brand-primary（暗=近白/亮=墨），字=foreground 反色
                d.bg(tk.text)
                    .text_color(tk.bg_base)
                    .hover(|s| s.opacity(0.9))
            } else {
                d.border_1()
                    .border_color(tk.border_l2)
                    .text_color(tk.text)
                    .hover(|s| s.bg(tk.hover))
            }
        })
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
}

/// h28 linkButton（web .linkButton：透明、12/18 tertiary、hover 白8%）。
fn link_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(28.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(14.0))
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(tk.text_3)
        .cursor_pointer()
        .hover(|s| s.bg(tk.hover).text_color(tk.text_2))
        .on_click(on_click)
        .child(label)
}

/// 添加区虚线按钮（web .addButton：等宽、h44、r12、l3 虚线、+ 图标）。
fn add_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .flex_1()
        .min_w(px(180.0))
        .h(px(44.0))
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .rounded(px(12.0))
        .border_1()
        .border_dashed()
        .border_color(tk.border_l3)
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text)
        .cursor_pointer()
        .hover(|s| s.bg(tk.hover))
        .on_click(on_click)
        .child(Icon::new(IconName::Plus).size(px(14.0)).text_color(tk.text_2))
        .child(label)
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
