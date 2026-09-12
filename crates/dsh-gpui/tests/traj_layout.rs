//! 轨迹台账行模型单测：分组/折叠/搜索重分段/行高几何。

use dsh_gpui::{traj_layout, TrajCell, TrajKind, TrajRow};

fn cell(
    turn: u64,
    kind: TrajKind,
    index: usize,
    step: Option<u64>,
    time: Option<u64>,
) -> TrajCell {
    TrajCell {
        turn,
        kind,
        index,
        text: format!("t{index}"),
        dim: false,
        step,
        detail_text: String::new(),
        reasoning: None,
        metrics: None,
        time_ms: time,
        tool: None,
    }
}

fn empty_sets() -> (std::collections::HashSet<u64>, std::collections::HashSet<usize>) {
    (Default::default(), Default::default())
}

/// 消息组（user 系连续合并）+ 步骤组（消息 + 其后工具行）+ 组头描述取时长和。
#[test]
fn groups_message_and_step() {
    let cells = vec![
        cell(1, TrajKind::User, 1, None, None),
        cell(1, TrajKind::User, 2, None, None),
        cell(1, TrajKind::Message, 3, Some(1), Some(6000)),
        cell(1, TrajKind::Tool, 4, None, Some(10)),
        cell(1, TrajKind::Tool, 5, None, Some(20)),
        cell(1, TrajKind::Message, 6, Some(2), Some(7000)),
        cell(1, TrajKind::Tool, 7, None, Some(30)),
    ];
    let (ct, ca) = empty_sets();
    let (rows, tops) = traj_layout(&cells, &ct, &ca);
    let kinds: Vec<String> = rows
        .iter()
        .map(|r| match r {
            TrajRow::Header(t) => format!("H{t}"),
            TrajRow::Group(title, desc) => format!("G{title}|{desc}"),
            TrajRow::Cell(i) => format!("C{i}"),
            TrajRow::TurnSummary(t, _) => format!("TS{t}"),
            TrajRow::AssistantSummary(i, _) => format!("AS{i}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "H1",
            "G消息|",
            "C0",
            "C1",
            "G步骤 1|6,030 毫秒",
            "C2",
            "C3",
            "C4",
            "G步骤 2|7,030 毫秒",
            "C5",
            "C6",
        ]
    );
    assert_eq!(rows.len(), tops.len());
    // 几何：轮头 52 / 组头 46 / 行 48 / 轮尾行 60
    assert_eq!(tops[1] - tops[0], 52.0);
    assert_eq!(tops[2] - tops[1], 46.0);
    assert_eq!(tops[3] - tops[2], 48.0);
    assert_eq!(tops[4] - tops[3], 48.0);
    assert!(tops.windows(2).all(|w| w[1] > w[0]));
}

/// 折叠轮：整轮换轮头 + 20px 摘要行（42 高含轮尾 padding）。
#[test]
fn collapsed_turn_summary() {
    let cells = vec![
        cell(1, TrajKind::User, 1, None, None),
        cell(1, TrajKind::Message, 2, Some(1), None),
        cell(1, TrajKind::Tool, 3, None, None),
    ];
    let mut ct = std::collections::HashSet::new();
    ct.insert(1);
    let (rows, tops) = traj_layout(&cells, &ct, &Default::default());
    assert!(matches!(
        &rows[..],
        [TrajRow::Header(1), TrajRow::TurnSummary(1, _)]
    ));
    assert_eq!(tops[1] - tops[0], 52.0);
    assert_eq!(tops.len(), 2);
}

/// 折叠助手：步骤组内消息 + 工具行 → 单条摘要行。
#[test]
fn collapsed_assistant_summary() {
    let cells = vec![
        cell(1, TrajKind::Message, 1, Some(1), None),
        cell(1, TrajKind::Tool, 2, None, None),
        cell(1, TrajKind::Tool, 3, None, None),
    ];
    let mut ca = std::collections::HashSet::new();
    ca.insert(1);
    let (rows, _) = traj_layout(&cells, &Default::default(), &ca);
    assert!(matches!(
        &rows[..],
        [
            TrajRow::Header(1),
            TrajRow::Group(_, _),
            TrajRow::AssistantSummary(1, _),
        ]
    ));
}

/// 搜索过滤后的切片重分段：跨轮过滤保留各自轮头。
#[test]
fn filtered_slice_resections() {
    let cells = vec![
        cell(1, TrajKind::Tool, 1, None, None),
        cell(2, TrajKind::Tool, 2, None, None),
    ];
    let (ct, ca) = empty_sets();
    let (rows, _) = traj_layout(&cells, &ct, &ca);
    let headers = rows
        .iter()
        .filter(|r| matches!(r, TrajRow::Header(_)))
        .count();
    assert_eq!(headers, 2);
}
