//! 外部框架层：三栏列宽契约 + 拖拽框架。
//!
//! 列宽数值对齐参考 `packages/client/ui-layout/src/client/columns.ts`；
//! 拖拽沿用 Zed redistributable_columns 的 on_drag/on_drag_move 模式
//! （起拖在把手，跟手在根容器，指针越出把手不丢事件）。

use gpui::*;

use crate::theme;


pub(crate) const SIDEBAR_MIN: f32 = 264.0;
pub(crate) const SIDEBAR_MAX: f32 = 420.0;
pub(crate) const SIDEBAR_DEFAULT: f32 = 280.0;
pub(crate) const SIDEBAR_COLLAPSED: f32 = 56.0;
pub(crate) const SIDEBAR_AUTO_COLLAPSE: f32 = 1024.0;
pub(crate) const CENTER_MIN: f32 = 640.0;
pub(crate) const DETAILS_MIN: f32 = 300.0;
pub(crate) const DETAILS_MAX: f32 = 520.0;
pub(crate) const DETAILS_DEFAULT: f32 = 360.0;

/// 会话内容列宽（ConversationRoot --dsh-chat-content-width）。
pub(crate) const CHAT_CONTENT_WIDTH: f32 = 748.0;
/// 输入卡上限 = 内容列 + 两侧 clearance 16px。
pub(crate) const COMPOSER_CARD_WIDTH: f32 = CHAT_CONTENT_WIDTH + 32.0;
/// 用户气泡宽度上限（MessageItem .userStack）。
pub(crate) const USER_BUBBLE_MAX: f32 = 525.0;

/// columns.ts 的「让步链」。
pub(crate) fn compute_columns(viewport: f32, sidebar_pref: f32, details_pref: f32) -> (f32, f32, f32) {
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
#[derive(Clone, Copy)]
pub(crate) enum DragSide {
    Sidebar,
    Details,
}

/// 列宽拖拽载荷（借鉴 Zed redistributable_columns 的 on_drag/on_drag_move
/// 模式：拖起后由整窗 on_drag_move 跟手，指针越出把手也不丢事件）。
#[derive(Clone, Copy)]
pub(crate) struct ColumnDrag {
    pub(crate) side: DragSide,
}
// --- 小组件 / 帮手 -----------------------------------------------------------

/// 列宽把手：`on_drag` 起拖（借鉴 Zed render_column_resize_divider），
/// 宽度由根容器上的 `on_drag_move::<ColumnDrag>` 按指针绝对位置跟手更新。
pub(crate) fn drag_handle(side: DragSide) -> Stateful<Div> {
    // 16px 命中带骑缝（±8px，起拖容错）；悬浮高亮画在中间的窄带
    // 上，恢复早期 8px 细高亮的观感。
    let group: SharedString = match side {
        DragSide::Sidebar => "drag-sidebar".into(),
        DragSide::Details => "drag-details".into(),
    };
    let group_h = group.clone();
    div()
        .id(group.clone())
        .w(px(16.0))
        .h_full()
        .flex_none()
        .mx(px(-8.0))
        .cursor_col_resize()
        .group(group_h)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_1()
                .right_1()
                .opacity(0.0)
                .group_hover(group.clone(), |s| s.opacity(1.0))
                .bg(theme::t().border_l2),
        )
        .on_drag(
            ColumnDrag { side },
            |_drag, _offset, _window, cx| cx.new(|_| gpui::Empty),
        )
}
