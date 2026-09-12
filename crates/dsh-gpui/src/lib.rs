//! dsh-gpui 库靶：轨迹台账纯逻辑（行模型/分组/折叠/行高几何）与消息块
//! 模型——从 bin 靶抽出以便集成测试直接驱动（bin 靶宏展开深度病态，
//! #[test] 无法在其中展开）。

/// 工具调用块（web 版 ToolRow）。
#[derive(Clone)]
pub struct ToolBlock {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub error: bool,
    pub open: bool,
    /// 读取/差异/搜索卡的 8 行折叠展开态（web 每实例 useState 的对应物）
    pub expanded: bool,
    /// 搜索卡里被折叠的文件组下标（升序；web collapsed Set 的对应物）
    pub collapsed_groups: Vec<usize>,
    /// 调用时长（调用所在 message → tool/result；轨迹台账时间列工具行）
    pub duration_ms: Option<u64>,
}

/// 台账行高规格（上游 TrajectoryCell/TrajectoryTurnHeader module.css 实值）。
pub const TRAJ_CELL_PX: f32 = 38.0;
pub const TRAJ_HEADER_PX: f32 = 44.0;
/// 轮头条内容道最大宽（上游 .inner max-width: 880px）。
pub const TRAJ_LANE_MAX_PX: f32 = 880.0;
/// 轮体规格（上游 TrajectoryTurn .body：gap 10、padding 8/16/22）。
pub const TRAJ_CELL_GAP_PX: f32 = 10.0;
pub const TRAJ_BODY_PAD_T_PX: f32 = 8.0;
pub const TRAJ_BODY_PAD_B_PX: f32 = 22.0;
/// 折叠摘要行高（上游 collapsed-summary td 20px）。
pub const TRAJ_SUMMARY_PX: f32 = 20.0;
/// 组头行高（上游 TrajectoryGroupHeader .root 36px）。
pub const TRAJ_GROUP_PX: f32 = 36.0;

/// 轨迹台账 cell（上游 TrajectoryCellProps 的 rustdsh 子集）。
pub struct TrajCell {
    pub turn: u64,
    pub kind: TrajKind,
    /// 全局序号（上游 #index，1 起）。
    pub index: usize,
    pub text: String,
    /// 回退摘要行（「仅工具调用」等），三级色渲染（上游 .toolCallOnly）。
    pub dim: bool,
    /// 轮内步号（步骤组分组键；user 系与工具行为 None）
    pub step: Option<u64>,
    /// 详情面板用原文（保留换行；text 是单行省略版）。
    pub detail_text: String,
    /// 思考块拼接（消息 cell 详情「思考」节）。
    pub reasoning: Option<String>,
    /// 消息行 usage 三指标（输入/输出/思考）。
    pub metrics: Option<(u64, u64, Option<u64>)>,
    /// 时长列（消息行 = 轮 llm 用时；rustdsh 日志无事件级时间戳，工具行恒 —）。
    pub time_ms: Option<u64>,
    pub tool: Option<ToolBlock>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TrajKind {
    User,
    Message,
    Tool,
}

/// 台账平坦行（uniform_list 虚拟化 item）：轮头条或 cell（cells 下标）。
#[derive(Clone)]
pub enum TrajRow {
    Header(u64),
    Cell(usize),
    /// Message / 步骤 N 组头（上游 TrajectoryGroupHeader，36px）
    Group(String, String),
    /// 折叠轮摘要行（上游 collapsedSummary turn，20px）
    TurnSummary(u64, String),
    /// 折叠助手摘要行（上游 collapsedSummary assistant，20px；携带消息 cell 序号）
    AssistantSummary(usize, String),
}

pub fn traj_layout(
    cells: &[TrajCell],
    collapsed_turns: &std::collections::HashSet<u64>,
    collapsed_assistants: &std::collections::HashSet<usize>,
) -> (Vec<TrajRow>, Vec<f32>) {
        let mut rows: Vec<TrajRow> = Vec::new();
        let mut tops: Vec<f32> = Vec::new();
        let mut y = 0.0f32;
        let mut last_turn: Option<u64> = None;
        let mut ci = 0usize;
        while ci < cells.len() {
            let c = &cells[ci];
            if last_turn != Some(c.turn) {
                rows.push(TrajRow::Header(c.turn));
                tops.push(y);
                y += TRAJ_HEADER_PX + TRAJ_BODY_PAD_T_PX;
                last_turn = Some(c.turn);
                // 折叠轮：整轮换一条 20px 摘要行
                if collapsed_turns.contains(&c.turn) {
                    let turn_cells = cells[ci..].iter().take_while(|n| n.turn == c.turn).count();
                    if turn_cells > 1 {
                        rows.push(TrajRow::TurnSummary(c.turn, c.text.clone()));
                        tops.push(y);
                        y += TRAJ_SUMMARY_PX + TRAJ_BODY_PAD_B_PX;
                        ci += turn_cells;
                        continue;
                    }
                }
            }
            // --- 组头（上游 Message / 步骤 N 组）：user 系 cell 归消息组
            // （连续合并）；带步号的消息 cell 连同其后工具行归步骤组 ---
            let is_msg_group_head =
                c.kind == TrajKind::User || (c.kind == TrajKind::Message && c.step.is_none());
            let is_step_head = c.kind == TrajKind::Message && c.step.is_some();
            let group_len = if is_step_head {
                1 + cells[ci + 1..]
                    .iter()
                    .take_while(|n| n.turn == c.turn && n.kind == TrajKind::Tool)
                    .count()
            } else if is_msg_group_head {
                cells[ci..]
                    .iter()
                    .take_while(|n| {
                        n.turn == c.turn
                            && (n.kind == TrajKind::User
                                || (n.kind == TrajKind::Message && n.step.is_none()))
                    })
                    .count()
                    .max(1)
            } else {
                // 孤儿工具行（理论上不出现）：单独成组
                1
            };
            let group_cells = &cells[ci..ci + group_len];
            let desc_ms: u64 = group_cells.iter().filter_map(|n| n.time_ms).sum();
            let title: String = if is_step_head {
                format!("步骤 {}", c.step.unwrap_or(1))
            } else {
                "消息".to_string()
            };
            rows.push(TrajRow::Group(
                title,
                if desc_ms > 0 {
                    format!(
                        "{} 毫秒",
                        desc_ms
                            .to_string()
                            .as_bytes()
                            .rchunks(3)
                            .rev()
                            .map(|b| std::str::from_utf8(b).unwrap_or_default())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                } else {
                    String::new()
                },
            ));
            tops.push(y);
            y += TRAJ_GROUP_PX + TRAJ_CELL_GAP_PX;
            // 组内行：助手折叠时消息行 + 其后工具行 → 单条摘要行
            let mut gi = 0usize;
            while gi < group_len {
                let gc = &cells[ci + gi];
                let tool_run = cells[ci + gi..]
                    .iter()
                    .skip(1)
                    .take_while(|n| n.kind == TrajKind::Tool)
                    .count();
                if gc.kind == TrajKind::Message
                    && tool_run > 0
                    && collapsed_assistants.contains(&gc.index)
                {
                    let last_in_turn = cells
                        .get(ci + gi + tool_run + 1)
                        .map(|n| n.turn != gc.turn)
                        .unwrap_or(true);
                    rows.push(TrajRow::AssistantSummary(gc.index, gc.text.clone()));
                    tops.push(y);
                    y += TRAJ_SUMMARY_PX
                        + if last_in_turn {
                            TRAJ_BODY_PAD_B_PX
                        } else {
                            TRAJ_CELL_GAP_PX
                        };
                    gi += tool_run + 1;
                    continue;
                }
                rows.push(TrajRow::Cell(ci + gi));
                tops.push(y);
                let last_in_turn = cells
                    .get(ci + gi + 1)
                    .map(|n| n.turn != gc.turn)
                    .unwrap_or(true);
                y += TRAJ_CELL_PX
                    + if last_in_turn {
                        TRAJ_BODY_PAD_B_PX
                    } else {
                        TRAJ_CELL_GAP_PX
                    };
                gi += 1;
            }
            ci += group_len;
        }
        (rows, tops)
    }
