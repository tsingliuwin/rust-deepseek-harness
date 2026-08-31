//! SidebarView — 左侧会话/工作区列表（独立 entity）。
//!
//! 拆分动机：菜单开合、搜索键入、分组折叠这类纯侧栏交互此前 notify 整个
//! AppView（聊天区可见项、composer、详情全随之重建）。侧栏交互态收进本
//! 视图后，这些操作只在侧栏内失效。
//!
//! 数据归属：sessions/workspaces/archived/current_id/current_workspace 与
//! 布局的 collapsed/width 以 AppView 为单一真相，经 `refresh` 推送快照；
//! 行菜单锚定用的列 bounds 是与 AppView 共享的 `Rc<RefCell>`（root 的
//! on_children_prepainted 捕获首子元素，这里消费）。动作闭包遵守
//! 「先侧栏本地 update、再宿主 update」的顺序，避免同名实体重入。

use crate::layout::{self, SIDEBAR_AUTO_COLLAPSE};
use crate::theme;
use crate::widgets::{icon_btn, rail_icon, session_row, tip};
use crate::{AppView, SessionMeta, WorkspaceInfo};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName, StyledExt};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

pub(crate) struct SidebarView {
    /// 宿主句柄（动作回写 + 数据快照来源）
    app: Entity<AppView>,
    // --- AppView 推送的数据快照（refresh 统一覆盖） ---
    sessions: Vec<SessionMeta>,
    workspaces: Vec<WorkspaceInfo>,
    archived: HashSet<String>,
    /// 当前列表高亮的会话 id
    current_id: dsh_llm::SessionId,
    current_workspace: Option<String>,
    /// 布局镜像（AppView::sidebar_collapsed / sidebar_width）
    layout_collapsed: bool,
    layout_width: f32,
    // --- 侧栏自有交互态 ---
    /// 折叠的工作区 id 集合（默认全部展开）
    collapsed_workspaces: HashSet<String>,
    /// 侧栏弹出的行菜单（会话 … / 工作区 … / 视图选项）
    sidebar_menu: Option<(String, String, f32)>,
    /// 视图选项触发按钮的窗口 bounds（菜单锚定）
    sb_anchor_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// 侧栏列的窗口 bounds（与 AppView 共享：root prepaint 捕获首子元素）
    sb_col_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// 搜索药丸的窗口 bounds（点外收起判定用）
    sb_search_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// 各行窗口 bounds（键 sbrow-{row_index}，每帧重建）：行菜单锚定行底
    sb_row_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// 搜索会话：展开 + 词条
    search_open: bool,
    search_query: String,
    search_input: Entity<InputState>,
    /// 视图选项：单列表 / 按工作区；排序 手动 / 最近更新
    group_flat: bool,
    order_manual: bool,
    _search_subscription: Subscription,
}

impl SidebarView {
    pub(crate) fn new(
        app: Entity<AppView>,
        search_input: Entity<InputState>,
        sb_col_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 搜索键入只打本视图：此前每个字符 notify 整个 AppView（聊天区可
        // 见项随每键重建）
        let search_subscription = cx.subscribe(&search_input, |sb, st, event, cx| {
            if matches!(event, InputEvent::Change) {
                let q = st.read_with(cx, |s, _| s.value().to_string());
                sb.search_query = q.trim().to_string();
                cx.notify();
            }
        });
        Self {
            app,
            sessions: Vec::new(),
            workspaces: Vec::new(),
            archived: Default::default(),
            current_id: dsh_llm::SessionId::new(String::new()),
            current_workspace: None,
            layout_collapsed: false,
            layout_width: layout::SIDEBAR_DEFAULT,
            collapsed_workspaces: Default::default(),
            sidebar_menu: None,
            sb_anchor_bounds: Rc::new(RefCell::new(None)),
            sb_col_bounds,
            sb_search_bounds: Rc::new(RefCell::new(None)),
            sb_row_bounds: Rc::new(RefCell::new(HashMap::new())),
            search_open: false,
            search_query: String::new(),
            search_input,
            group_flat: false,
            order_manual: false,
            _search_subscription: search_subscription,
        }
    }

    /// 宿主数据推送（AppView 在 sessions/workspaces/归档/选中/布局变更后
    /// 调用；快照整体覆盖，无增量一致性负担）。
    pub(crate) fn refresh(
        &mut self,
        sessions: Vec<SessionMeta>,
        workspaces: Vec<WorkspaceInfo>,
        archived: HashSet<String>,
        current_id: dsh_llm::SessionId,
        current_workspace: Option<String>,
        layout_collapsed: bool,
        layout_width: f32,
    ) {
        self.sessions = sessions;
        self.workspaces = workspaces;
        self.archived = archived;
        self.current_id = current_id;
        self.current_workspace = current_workspace;
        self.layout_collapsed = layout_collapsed;
        self.layout_width = layout_width;
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vw: f32 = window.viewport_size().width.into();
        let collapsed = self.layout_collapsed || vw < SIDEBAR_AUTO_COLLAPSE;
        let width = self.layout_width;
        // 双句柄：this = 宿主动作；sb = 侧栏本地态
        let this = self.app.clone();
        let sb = cx.entity();
        let current_id = self.current_id.clone();

        let mut col = div()
            .h_full()
            .w(px(width))
            .flex_none()
            .bg(theme::t().sidebar_bg)
            .border_r_1()
            .border_color(theme::t().border_l1);
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
                    // web rail：折叠态 toggle 呈现鲸鱼标记
                    div()
                        .id("sb-expand")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::t().hover))
                        .tooltip(tip("展开侧边栏"))
                        .on_click(move |_, _, cx| {
                            t_expand.update(cx, |v, cx| { v.sidebar_collapsed = false; v.refresh_sidebar(cx); });
                        })
                        .child(
                            gpui::svg()
                                .path("brands/fish.svg")
                                .w(px(24.0)).h(px(17.65))
                                .text_color(theme::t().text),
                        ),
                )
                .child(
                    rail_icon("sb-new", IconName::Plus, "新建会话", move |_, _, cx| {
                        t_new.update(cx, |v, cx| { v.new_session(cx); });
                    }),
                )
                .child(div().flex_grow())
                .child(
                    rail_icon("sb-settings", IconName::Settings, "设置", move |_, _, cx| {
                        t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                    }),
                );
        } else {
            let t_collapse = this.clone();
            let t_new = this.clone();
            let t_settings = this.clone();
            // 分组构建：每个工作区 = 34px 行（folder/title/折叠 chevron/
            //   hover +/…）+ 展开时的会话行；未分组会话排末尾。
            let mut all_rows: Vec<AnyElement> = Vec::new();
            let mut row_index = 0usize;
            // 行 bounds 表每帧重建（prepaint 填、点击回调读）：行菜单锚定
            // 行底，位置不随点击落点漂移、免疫滚动
            let row_bounds = self.sb_row_bounds.clone();
            row_bounds.borrow_mut().clear();
            let shell_bounds = row_bounds.clone();
            let anchored_row = move |key: String, row: AnyElement| -> AnyElement {
                let kb = key;
                let sb_bounds = shell_bounds.clone();
                div()
                    .on_children_prepainted(move |children, _, _| {
                        if let Some(b) = children.first().cloned() {
                            sb_bounds.borrow_mut().insert(kb.clone(), b);
                        }
                    })
                    .child(row)
                    .into_any_element()
            };
            let search_active = !self.search_query.is_empty();
            if search_active {
                // web searchTree：匹配会话平铺（工作区行常规渲染跳过）
                let needle = self.search_query.to_lowercase();
                for meta in &self.sessions {
                    if self.archived.contains(meta.id.as_str()) {
                        continue;
                    }
                    if !meta.title.to_lowercase().contains(&needle) {
                        continue;
                    }
                    let t_sw = this.clone();
                    let id = meta.id.clone();
                    let t_m = sb.clone();
                    let id_m = meta.id.as_str().to_string();
                    let key = format!("sbrow-{row_index}");
                    let kb = key.clone();
                    let slot = row_bounds.clone();
                    let active = meta.id == current_id;
                    all_rows.push(anchored_row(
                        key,
                        session_row(row_index, meta.title.clone(), meta.time_label.clone(), active, meta.blank, move |_, _, cx| {
                            let id = id.clone();
                            t_sw.update(cx, |v, cx| { v.switch_session(id, cx); });
                        }, move |click, _, cx| {
                            let id = id_m.clone();
                            let y = slot.borrow().get(kb.as_str())
                                .map(|b| f32::from(b.origin.y + b.size.height))
                                .unwrap_or_else(|| crate::click_anchor_y(click));
                            t_m.update(cx, |v, cx| {
                                v.sidebar_menu = Some(("session".into(), id, y));
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    ));
                    row_index += 1;
                }
            }
            for w in &self.workspaces {
                if search_active && !self.group_flat {
                    // 搜索命中：不发工作区行（会话已平铺）
                }
                let wid = w.id.clone();
                let wtitle = w.title.clone();
                let wid3 = wid.clone();
                let wcollapsed = self.collapsed_workspaces.contains(&wid);
                // --- 工作区行 ---
                {
                    let t_toggle = sb.clone();
                    let t_plus = this.clone();
                    let t_sb_uncollapse = sb.clone();
                    let t_more = sb.clone();
                    let group: SharedString = format!("ws-{wid}").into();
                    let g2 = group.clone();
                    all_rows.push(anchored_row(
                        format!("sbrow-{row_index}"),
                        div()
                            .id(SharedString::from(format!("ws-row-{wid}")))
                            .group(group)
                            .h(px(34.0))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::t().hover))
                            .on_click(move |_, _, cx| {
                                let id = wid3.clone();
                                t_toggle.update(cx, |v, cx| {
                                    if !v.collapsed_workspaces.remove(&id) {
                                        v.collapsed_workspaces.insert(id);
                                    }
                                    cx.notify();
                                });
                            })
                            .child(Icon::new(IconName::Folder).size(px(16.0)).text_color(theme::t().accent))
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
                                    .child(wtitle.clone()),
                            )
                            .child(
                                // hover 出的操作（/…）
                                div()
                                    .id(SharedString::from(format!("ws-actions-{wid}")))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .opacity(0.0)
                                    .group_hover(g2, |s| s.opacity(1.0))
                                    .child({
                                        let w = wid.clone();
                                        let mut b = div()
                                            .id(SharedString::from(format!("ws-plus-{w}")))
                                            .size(px(16.0))
                                            .rounded(px(4.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::t().active))
                                            // 行内按钮阻断冒泡：点 ＋ 不应同时
                                            // 折叠/展开工作区
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                let id = w.clone();
                                                // 先侧栏本地（收起组），再宿主
                                                t_sb_uncollapse.update(cx, |v, _| {
                                                    v.collapsed_workspaces.remove(&id);
                                                });
                                                t_plus.update(cx, |v, cx| {
                                                    v.current_workspace = Some(id.clone());
                                                    v.sync_fs_sandbox();
                                                    v.new_session(cx);
                                                });
                                            });
                                        b = b.child(Icon::new(IconName::Plus).size(px(12.0)).text_color(theme::t().text_2));
                                        b
                                    })
                                    .child({
                                        let w = wid.clone();
                                        let key = format!("sbrow-{row_index}");
                                        let kb = key.clone();
                                        let slot = row_bounds.clone();
                                        div()
                                            .id(SharedString::from(format!("ws-more-{w}")))
                                            .size(px(16.0))
                                            .rounded(px(4.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::t().active))
                                            .on_click(move |click, _, cx| {
                                                // 行内按钮阻断冒泡：点 … 只开菜单，
                                                // 不同时触发工作区折叠/展开
                                                cx.stop_propagation();
                                                let w = w.clone();
                                                let y = slot.borrow().get(kb.as_str())
                                                    .map(|b| f32::from(b.origin.y + b.size.height))
                                                    .unwrap_or_else(|| crate::click_anchor_y(click));
                                                t_more.update(cx, |v, cx| {
                                                    v.sidebar_menu = Some(("ws".into(), w, y));
                                                    cx.notify();
                                                });
                                            })
                                            .child(Icon::new(IconName::Ellipsis).size(px(12.0)).text_color(theme::t().text_2))
                                    }),
                            )
                            .child(Icon::new(if wcollapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                                .size(px(12.0))
                                .text_color(theme::t().caption))
                            .into_any_element(),
                    ));
                    row_index += 1;
                }
                // --- 该组会话行 ---
                if !wcollapsed && !self.group_flat {
                    // web 语义：会话按其 project cwd 归组（目录即真相）
                    let w_key = dsh_persist::project_key(&w.path);
                    let members: Vec<&SessionMeta> = self
                        .sessions
                        .iter()
                        .filter(|m| {
                            !self.archived.contains(m.id.as_str())
                                && m.cwd
                                    .as_deref()
                                    .map(|c| dsh_persist::project_key(c) == w_key)
                                    .unwrap_or(false)
                        })
                        .collect();
                    let w_sids: Vec<String> = if self.order_manual {
                        let manual: Vec<String> = w.session_ids.clone();
                        let mut ordered: Vec<String> =
                            manual.into_iter().filter(|s| members.iter().any(|m| m.id.as_str() == s)).collect();
                        for m in &members {
                            if !ordered.iter().any(|s| s == m.id.as_str()) {
                                ordered.push(m.id.as_str().to_string());
                            }
                        }
                        ordered
                    } else {
                        members.iter().map(|m| m.id.as_str().to_string()).collect()
                    };
                    for sid in &w_sids {
                        if let Some(meta) = self.sessions.iter().find(|m| m.id.as_str() == sid) {
                            let t_sw = this.clone();
                            let id = meta.id.clone();
                            let active = meta.id == current_id;
                            {
                                let t_m = sb.clone();
                                let id_m = meta.id.as_str().to_string();
                                let key = format!("sbrow-{row_index}");
                                let kb = key.clone();
                                let slot = row_bounds.clone();
                                all_rows.push(anchored_row(
                                    key,
                                    session_row(row_index, meta.title.clone(), meta.time_label.clone(), active, meta.blank, move |_, _, cx| {
                                        let id = id.clone();
                                        t_sw.update(cx, |v, cx| { v.switch_session(id, cx); });
                                    }, move |click, _, cx| {
                                        let id = id_m.clone();
                                        let y = slot.borrow().get(kb.as_str())
                                            .map(|b| f32::from(b.origin.y + b.size.height))
                                            .unwrap_or_else(|| crate::click_anchor_y(click));
                                        t_m.update(cx, |v, cx| {
                                            v.sidebar_menu = Some(("session".into(), id, y));
                                            cx.notify();
                                        });
                                    })
                                    .into_any_element(),
                                ));
                            }
                            row_index += 1;
                        }
                    }
                }
            }
            // 未分组会话：cwd 缺失或不属于任何工作区
            let ws_keys: std::collections::HashSet<String> = self
                .workspaces
                .iter()
                .map(|w| dsh_persist::project_key(&w.path))
                .collect();
            for meta in self.sessions.iter().filter(|m| {
                !self.archived.contains(m.id.as_str())
                    && m.cwd
                        .as_deref()
                        .map(|c| !ws_keys.contains(&dsh_persist::project_key(c)))
                        .unwrap_or(true)
            }) {
                let t_sw = this.clone();
                let id = meta.id.clone();
                let active = meta.id == current_id;
                {
                    let t_m = sb.clone();
                    let id_m = meta.id.as_str().to_string();
                    let key = format!("sbrow-{row_index}");
                    let kb = key.clone();
                    let slot = row_bounds.clone();
                    all_rows.push(anchored_row(
                        key,
                        session_row(row_index, meta.title.clone(), meta.time_label.clone(), active, meta.blank, move |_, _, cx| {
                            let id = id.clone();
                            t_sw.update(cx, |v, cx| { v.switch_session(id, cx); });
                        }, move |click, _, cx| {
                            let id = id_m.clone();
                            let y = slot.borrow().get(kb.as_str())
                                .map(|b| f32::from(b.origin.y + b.size.height))
                                .unwrap_or_else(|| crate::click_anchor_y(click));
                            t_m.update(cx, |v, cx| {
                                v.sidebar_menu = Some(("session".into(), id, y));
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    ));
                }
                row_index += 1;
            }
            let t_search_toggle = sb.clone();
            let t_search_clear = sb.clone();
            let search_input_state = self.search_input.clone();
            let t_view_menu = sb.clone();
            let t_add_ws = this.clone();
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
                                .child(
                                    // 官方鲸鱼标记（web FishLogo，currentColor 随主题）
                                    gpui::svg()
                                        .path("brands/fish.svg")
                                        .w(px(24.0)).h(px(17.65))
                                        .text_color(theme::t().text),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_BRAND))
                                        .line_height(px(24.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("deepseek"),
                                )
                                .child(
                                    // 徽牌（web buildRevision：品牌色底 + 反色字）
                                    div()
                                        .px(px(4.0))
                                        .rounded(px(3.0))
                                        .bg(theme::t().text)
                                        .text_color(theme::t().bg_base)
                                        .font_family(crate::widgets::theme_mono())
                                        .text_size(px(8.0))
                                        .line_height(px(16.0))
                                        .child("HARNESS"),
                                ),
                        )
                        .child(
                            icon_btn("sb-collapse", IconName::PanelLeftClose, theme::t().text_2, "收起侧边栏", move |_, _, cx| {
                                t_collapse.update(cx, |v, cx| { v.sidebar_collapsed = true; v.refresh_sidebar(cx); });
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
                        .border_color(theme::t().border_l2)
                        .bg(theme::t().surface)
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::t().text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::t().surface_2))
                        .on_click(move |_, _, cx| {
                            t_new.update(cx, |v, cx| { v.new_session(cx); });
                        })
                        .child(Icon::new(IconName::Plus).size(px(16.0)))
                        .child("新建会话"),
                )
                .child(
                    // 区块头：会话 + 搜索 / 视图 / 新建工作区（web .sectionHeader）
                    div()
                        .h(px(36.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .pl_1()
                        .mb_1()
                        .when(!self.search_open, |hdr| {
                            hdr.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(theme::FONT_ROW))
                                    .line_height(px(20.0))
                                    .text_color(theme::t().text_3)
                                    .child("工作区"),
                            )
                            .child(
                                icon_btn("sb-search", IconName::Search, theme::t().text_2, "搜索会话", move |_, _, cx| {
                                    t_search_toggle.update(cx, |v, cx| {
                                        v.search_open = true;
                                        cx.notify();
                                    });
                                }),
                            )
                        })
                        .when(self.search_open, |hdr| {
                            // web .searchExpanded：动作簇（视图选项 / 添加
                            // 工作区）整体让位，药丸独占头行。展开动效对齐
                            // web 的 180ms ease-in-out：药丸从 28px 圆钮长到
                            // 整行，输入与清除钮同步淡入（gpui 无 CSS 过渡，
                            // 用 with_animation 补）
                            let search_bounds = self.sb_search_bounds.clone();
                            let pill_full = (width - 28.0).max(120.0);
                            hdr.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .on_children_prepainted(move |children, _, _| {
                                        *search_bounds.borrow_mut() = children.first().cloned();
                                    })
                                    .child(
                                        div()
                                            .id("sb-search-pill")
                                            .h(px(30.0))
                                            .flex()
                                            .items_center()
                                            .overflow_hidden()
                                            .pr(px(4.0))
                                            .rounded(px(10.0))
                                            .border_1()
                                            .border_color(theme::t().border_l2)
                                            .child(
                                                div()
                                                    .w(px(28.0))
                                                    .h(px(30.0))
                                                    .flex_none()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        Icon::new(IconName::Search)
                                                            .size(px(11.0))
                                                            .text_color(theme::t().caption),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .items_center()
                                                    .child(
                                                        Input::new(&self.search_input)
                                                            .appearance(false)
                                                            .flex_1()
                                                            .min_w_0()
                                                            .text_size(px(theme::FONT_TAB)),
                                                    )
                                                    .child({
                                                        div()
                                                            .id("sb-search-clear")
                                                            .w(px(24.0))
                                                            .h(px(24.0))
                                                            .flex_none()
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .rounded_full()
                                                            .cursor_pointer()
                                                            .text_color(theme::t().text_2)
                                                            .hover(|s| s.bg(theme::t().hover))
                                                            .on_click(move |_, window, cx| {
                                                                t_search_clear.update(cx, |v, cx| {
                                                                    v.search_open = false;
                                                                    v.search_query.clear();
                                                                    search_input_state.update(cx, |s, cx| {
                                                                        s.set_value("", window, cx);
                                                                    });
                                                                    cx.notify();
                                                                });
                                                            })
                                                            .child(Icon::new(IconName::Close).size(px(14.0)))
                                                    })
                                                    .with_animation(
                                                        "sb-search-fade",
                                                        Animation::new(Duration::from_millis(
                                                            120,
                                                        )),
                                                        |el, delta| el.opacity(delta),
                                                    ),
                                            )
                                            .with_animation(
                                                "sb-search-expand",
                                                Animation::new(Duration::from_millis(180))
                                                    .with_easing(ease_in_out),
                                                move |pill, delta| {
                                                    pill.w(px(28.0 + (pill_full - 28.0) * delta))
                                                },
                                            ),
                                    ),
                            )
                        })
                        .when(!self.search_open, |hdr| {
                            // web .headerActions：搜索展开时整簇隐藏
                            hdr.child({
                                let anchor_bounds = self.sb_anchor_bounds.clone();
                                // 包一层捕获按钮窗口 bounds（菜单锚定基准）
                                div()
                                    .on_children_prepainted(move |children, _, _| {
                                        *anchor_bounds.borrow_mut() = children.first().cloned();
                                    })
                                    .child(icon_btn("sb-view", IconName::Ellipsis, theme::t().text_2, "视图选项", move |_, _, cx| {
                                        t_view_menu.update(cx, |v, cx| {
                                            v.sidebar_menu = Some(("view".into(), String::new(), 96.0));
                                            cx.notify();
                                        });
                                    }))
                            })
                            .child(
                                icon_btn("sb-add-workspace", IconName::Plus, theme::t().text_2, "添加工作区", move |_, window, cx| {
                                    // 异步目录拾取：Task 丢弃即取消，必须 detach。
                                    // 不挂父窗：rfd set_parent 会调 gpui Window 的
                                    // display_handle()，0.2.2 Windows 后端是
                                    // unimplemented!()，点击即崩（panic 绕过 rfd
                                    // 的 .ok() 容错）；无主对话框在 Windows 上安全。
                                    let _ = window;
                                    let t = t_add_ws.clone();
                                    let dialog = rfd::AsyncFileDialog::new();
                                    cx.spawn(move |acx: &mut AsyncApp| {
                                        let mut acx = acx.clone();
                                        async move {
                                            let picked = dialog.pick_folder().await;
                                            if let Some(folder) = picked {
                                                let p = folder.path().to_string_lossy().to_string();
                                                let _ = t.update(&mut acx, |v, cx| {
                                                    if !v.workspaces.iter().any(|w| w.path == p) {
                                                        v.create_workspace(p, cx);
                                                    }
                                                });
                                            }
                                        }
                                    })
                                    .detach();
                                })
                            )
                        }),
                )
                .child(
                    // 会话列表 + 底部渐隐（web .fade）
                    div()
                        .relative()
                        .flex_grow()
                        .min_h_0()
                        .child(
                            div()
                                .id("sidebar-list")
                                .h_full()
                                .overflow_y_scroll()
                                .v_flex()
                                .gap_0p5()
                                .children(all_rows),
                        )
                        .child(
                            // 底部渐隐（web .fade）：起点必须是与 sidebar_bg 同
                            // 色但 alpha=0 的颜色——若用 transparent_black，浅色
                            // 侧栏上插值的中间像素会变灰，呈现一条黑带。
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(px(24.0))
                                .bg(linear_gradient(
                                    180.0,
                                    linear_color_stop(
                                        {
                                            let sbg = theme::t().sidebar_bg;
                                            gpui::Rgba { r: sbg.r, g: sbg.g, b: sbg.b, a: 0.0 }
                                        },
                                        0.0,
                                    ),
                                    linear_color_stop(theme::t().sidebar_bg, 1.0),
                                )),
                        ),
                )
                .when_some(self.sidebar_menu.clone(), |col, (kind, target, y)| {
                    // 点外关闭（同 hero 面板）：定位壳铺满侧栏来承接菜单
                    // （taffy 绝对定位以直接父容器为基准），菜单 bounds 经
                    // prepaint 捕获；canvas 在 paint 阶段注册窗口级 mousedown，
                    // Bubble 相且落点在菜单外才收起。
                    let menu_bounds = std::sync::Arc::new(std::sync::Mutex::new(None::<Bounds<Pixels>>));
                    let t_dismiss = sb.clone();
                    // 视图菜单锚定触发按钮（web portal align=end：按钮下方
                    // 4px、右缘对齐按钮右缘）；行菜单（ws/session）保持
                    // 旧的全宽位。卡规格：min 218 / max 360、r12、
                    // inverted 发丝边、specific-menu 底（web .list）
                    let anchor = if kind == "view" {
                        match (
                            self.sb_anchor_bounds.borrow().clone(),
                            self.sb_col_bounds.borrow().clone(),
                        ) {
                            (Some(a), Some(c)) => {
                                // web place() 的视口夹取：x = 按钮右缘 - 菜单宽，
                                // 两侧留 12px。菜单实际宽即 min-width 218（内容
                                // 不超）；侧栏过窄时左缘夹在 12px，右缘溢进中栏
                                // 而不是被窗口裁掉
                                let btn_right = f32::from(a.origin.x + a.size.width - c.origin.x);
                                let menu_w = 218.0;
                                let upper = (vw - menu_w - 12.0).max(12.0);
                                Some((
                                    f32::from(a.origin.y + a.size.height - c.origin.y) + 4.0,
                                    (btn_right - menu_w).clamp(12.0, upper),
                                ))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    col.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .on_children_prepainted({
                                let mb = menu_bounds.clone();
                                move |children, _, _| {
                                    *mb.lock().unwrap() = children.first().cloned();
                                }
                            })
                            .child(
                        div()
                            .id("sb-menu")
                            .absolute()
                            // 不透明卡面：遮住后方元素的 hover/点击
                            .occlude()
                            .map(|d| {
                                if let Some((top, left)) = anchor {
                                    d.top(px(top)).left(px(left))
                                } else {
                                    // 行菜单（ws/session）：y 是触发行的窗口
                                    // 底缘（prepaint 捕获的行 bounds），换算成
                                    // 列内 top 再留 4px 间隙——位置恒定，免疫
                                    // 列表滚动与点击落点漂移
                                    let top = match self.sb_col_bounds.borrow().clone() {
                                        Some(c) => y - f32::from(c.origin.y) + 4.0,
                                        None => y,
                                    };
                                    d.top(px(top)).left(px(8.0)).right(px(8.0))
                                }
                            })
                            .min_w(px(218.0))
                            .max_w(px(360.0))
                            .v_flex()
                            .p(px(4.0))
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(theme::t().border_inverted)
                            .bg(theme::t().menu)
                            .shadow_lg()
                            .children(if kind == "view" {
                                // 视图选项（web ViewOptionsMenu：分组 label + 单选 + 分隔 + 排序）
                                let t1 = sb.clone();
                                let t2 = sb.clone();
                                let t3 = sb.clone();
                                let t4 = sb.clone();
                                vec![
                                    crate::sb_menu_label("分组方式").into_any_element(),
                                    crate::sb_menu_check_row(
                                        "view-group-ws",
                                        "按工作区",
                                        !self.group_flat,
                                        move |_, _, cx| t1.update(cx, |v, cx| {
                                            v.group_flat = false;
                                            v.sidebar_menu = None;
                                            cx.notify();
                                        }),
                                    ).into_any_element(),
                                    crate::sb_menu_check_row(
                                        "view-group-flat",
                                        "单列表",
                                        self.group_flat,
                                        move |_, _, cx| t2.update(cx, |v, cx| {
                                            v.group_flat = true;
                                            v.sidebar_menu = None;
                                            cx.notify();
                                        }),
                                    ).into_any_element(),
                                    crate::sb_menu_divider().into_any_element(),
                                    crate::sb_menu_label("排序方式").into_any_element(),
                                    crate::sb_menu_check_row(
                                        "view-order-manual",
                                        "手动排序",
                                        self.order_manual,
                                        move |_, _, cx| t3.update(cx, |v, cx| {
                                            v.order_manual = true;
                                            v.sidebar_menu = None;
                                            cx.notify();
                                        }),
                                    ).into_any_element(),
                                    crate::sb_menu_check_row(
                                        "view-order-updated",
                                        "最近更新",
                                        !self.order_manual,
                                        move |_, _, cx| t4.update(cx, |v, cx| {
                                            v.order_manual = false;
                                            v.sidebar_menu = None;
                                            cx.notify();
                                        }),
                                    ).into_any_element(),
                                ]
                            } else if kind == "ws" {
                                let t1 = sb.clone();
                                let t2 = sb.clone();
                                let t_app1 = this.clone();
                                let t_app2 = this.clone();
                                let id1 = target.clone();
                                let id2 = target.clone();
                                vec![
                                    crate::sb_menu_row("ws-rename", "重命名", false, move |_, _, cx| {
                                        let id = id1.clone();
                                        t1.update(cx, |v, cx| { v.sidebar_menu = None; cx.notify(); });
                                        t_app1.update(cx, |v, cx| {
                                            v.renaming_workspace = Some(id);
                                            cx.notify();
                                        });
                                    }).into_any_element(),
                                    crate::sb_menu_row("ws-delete", "删除工作区", true, move |_, _, cx| {
                                        let id = id2.clone();
                                        t2.update(cx, |v, cx| { v.sidebar_menu = None; cx.notify(); });
                                        t_app2.update(cx, |v, cx| {
                                            v.delete_workspace(&id);
                                            v.refresh_sidebar(cx);
                                        });
                                    }).into_any_element(),
                                ]
                            } else {
                                // web Rows 会话菜单三项：rename / fork / archive
                                let t_m = sb.clone();
                                let t_app1 = this.clone();
                                let t_app2 = this.clone();
                                let t_app3 = this.clone();
                                let id1 = target.clone();
                                let id2 = target.clone();
                                let id3 = target.clone();
                                let title = self
                                    .sessions
                                    .iter()
                                    .find(|m| m.id.as_str() == target)
                                    .map(|m| m.title.clone())
                                    .unwrap_or_default();
                                vec![
                                    crate::sb_menu_row("sess-rename", "重命名", false, move |_, window, cx| {
                                        let id = id1.clone();
                                        let title = title.clone();
                                        t_m.update(cx, |v, cx| { v.sidebar_menu = None; cx.notify(); });
                                        t_app1.update(cx, |v, cx| {
                                            v.renaming_session = Some(id);
                                            v.rename_input.update(cx, |s, cx| {
                                                s.set_value(&title, window, cx);
                                            });
                                            cx.notify();
                                        });
                                    }).into_any_element(),
                                    crate::sb_menu_row("sess-fork", "分叉会话", false, move |_, _, cx| {
                                        let id = id2.clone();
                                        let sid = dsh_llm::SessionId::new(id);
                                        t_app2.update(cx, |v, cx| {
                                            v.fork_session(&sid, cx);
                                        });
                                    }).into_any_element(),
                                    // web：归档不触碰日志，不作破坏性标红
                                    crate::sb_menu_row("sess-archive", "归档会话", false, move |_, _, cx| {
                                        let id = id3.clone();
                                        let sid = dsh_llm::SessionId::new(id);
                                        t_app3.update(cx, |v, cx| {
                                            v.archive_session(&sid, cx);
                                        });
                                    }).into_any_element(),
                                ]
                            }),
                        ),
                    )
                    .child(
                        // canvas 必须脱流（同 hero：gap 型 v_flex 会多一条间隙）
                        div().absolute().child(canvas(
                            move |_, _, _| {},
                            move |_, _, window, _| {
                                let bounds = menu_bounds
                                    .lock()
                                    .unwrap()
                                    .clone()
                                    .unwrap_or_default();
                                window.on_mouse_event(
                                    move |event: &MouseDownEvent, phase: DispatchPhase, _, cx| {
                                        // Capture 相：先于元素回调关闭旧菜单——
                                        // 点另一行的 … 时新菜单随后被元素回调
                                        // 打开，一次点击即完成「移动」
                                        if phase == DispatchPhase::Capture
                                            && !bounds.contains(&event.position)
                                        {
                                            t_dismiss.update(cx, |v, cx| {
                                                if v.sidebar_menu.is_some() {
                                                    v.sidebar_menu = None;
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    },
                                );
                            },
                        )),
                    )
                })
                .when(self.search_open, |col| {
                    // web WorkspaceBrowser 外点收起：点搜索药丸之外先失焦，
                    // 查询为空才收起；非空保持展开（结果仍可见）
                    let search_bounds = self.sb_search_bounds.clone();
                    let t = sb.clone();
                    col.child(
                        div().absolute().child(canvas(
                            move |_, _, _| {},
                            move |_, _, window, _| {
                                let bounds = search_bounds.borrow().clone().unwrap_or_default();
                                window.on_mouse_event(
                                    move |event: &MouseDownEvent, phase: DispatchPhase, _, cx| {
                                        if phase == DispatchPhase::Bubble
                                            && !bounds.contains(&event.position)
                                        {
                                            t.update(cx, |v, cx| {
                                                if v.search_open
                                                    && v.search_query.trim().is_empty()
                                                {
                                                    v.search_open = false;
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    },
                                );
                            },
                        )),
                    )
                })
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
                        .hover(|s| s.bg(theme::t().hover))
                        .on_click(move |_, _, cx| {
                            t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                        })
                        .child(Icon::new(IconName::Settings).size(px(16.0)).text_color(theme::t().text_3))
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::t().text_2)
                                .child("设置"),
                        ),
                );
        }
        col
    }
}
