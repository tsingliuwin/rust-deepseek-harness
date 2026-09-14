//! 运行中查看式切换的状态机（ViewSplit）语义回归：显示路由 / composer
//! 让位 / 轮终收敛三条判定在完整时序下的取值。

use dsh_gpui::ViewSplit;

#[test]
fn running_in_view_routes_and_keeps_composer() {
    let mut s = ViewSplit::default();
    s.on_turn_start();
    assert!(s.routes_to_view());
    assert!(!s.composer_inert());
    assert!(!s.needs_commit());
}

#[test]
fn peek_during_run_drops_events_and_inerts_composer() {
    let mut s = ViewSplit::default();
    s.on_turn_start();
    s.peek();
    assert!(!s.routes_to_view(), "peek 中运行会话事件不得进视图");
    assert!(s.composer_inert(), "异会话运行中 composer 让位");
    assert!(!s.needs_commit(), "运行未结束不收敛");
}

#[test]
fn turn_end_while_peeking_commits_once() {
    let mut s = ViewSplit::default();
    s.on_turn_start();
    s.peek();
    assert!(!s.routes_to_view());
    assert!(s.on_turn_end(), "轮终须收敛回视图会话");
    // 收敛动作由调用方执行（AppView::commit_peek → settle）：动作完成前
    // 事件仍不进视图（视图还停在别处）；提交不重复触发
    assert!(!s.routes_to_view());
    s.settle();
    assert!(s.routes_to_view(), "收敛后事件恢复进视图");
    assert!(!s.composer_inert());
    assert!(!s.needs_commit());
    assert!(!s.on_turn_end());
}

#[test]
fn turn_end_in_view_needs_no_commit() {
    let mut s = ViewSplit::default();
    s.on_turn_start();
    assert!(!s.on_turn_end());
    assert!(!s.needs_commit());
}

#[test]
fn idle_peek_defers_to_send_and_turn_start_blocks_commit() {
    let mut s = ViewSplit::default();
    s.peek();
    assert!(!s.composer_inert(), "agent 空闲的 peek 不置 composer 让位");
    assert!(s.needs_commit(), "空闲 peek 是发送前的收敛时机");
    // 边界反向：peek 中收到 TurnStarted（新轮开始）不得收敛——收敛会把
    // agent 从运行会话切走，正是本模型要防止的损坏路径。
    s.on_turn_start();
    assert!(s.composer_inert());
    assert!(!s.needs_commit());
}
