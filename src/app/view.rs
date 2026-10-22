//! 局面 → 视图投影（共享）：快照的中性结构化投影。
//!
//! 收敛 banqi-gui/src/state.rs（`extract_game_state`）与 banqi-server/src/state.rs
//! （`view()`）的重复实现：
//! - [`project`] / [`game_view`]：中性结构化投影（格位状态、棋子归属、
//!   终局原因、棋谱），字符串格式化由调用方决定；
//! - [`GameStateView`] / [`game_state_view`]：前端契约状态视图（snake_case，
//!   棋盘短码 + 棋子全名 + 位板/动作掩码/翻子概率），server 状态 DTO 与
//!   MCTS 节点棋盘共用，前端零适配直传。

use banqi_core::core::env::types::{Piece, PieceType, Player};
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot};

use crate::app::common::history_entries;

// 格位与棋盘投影

/// 格位状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Empty,
    Hidden,
    Revealed,
}

/// 单个格位的中性视图；`Revealed` 时 `piece` 必有值。
#[derive(Debug, Clone, Copy)]
pub struct BoardCell {
    pub state: CellState,
    pub piece: Option<Piece>,
}

/// 快照的棋盘级中性投影（暗子身份经 `hidden` 不泄露——调用方自行决定展示口径）。
#[derive(Debug, Clone)]
pub struct BoardProjection {
    /// 全部格位（行优先，长度 = rows × cols）。
    pub cells: Vec<BoardCell>,
    /// `[红方阵亡, 黑方阵亡]`：按 `dead_counts` 展开为逐枚棋子类型。
    pub dead: [Vec<PieceType>; 2],
    /// `[红方未翻, 黑方未翻]`：`hidden_bag` 按归属方过滤。
    pub hidden: [Vec<Piece>; 2],
    pub current_player: Player,
    /// 连续无吃子步数
    pub move_counter: u32,
    /// 总步数
    pub total_step_counter: u32,
}

/// 投影快照为棋盘级中性视图。
pub fn project(snap: &PositionSnapshot) -> BoardProjection {
    use banqi_core::core::env::Slot::*;
    let cells: Vec<BoardCell> = snap
        .slots
        .iter()
        .map(|slot| match slot {
            Empty => BoardCell { state: CellState::Empty, piece: None },
            Hidden => BoardCell { state: CellState::Hidden, piece: None },
            Revealed(p) => BoardCell { state: CellState::Revealed, piece: Some(*p) },
        })
        .collect();

    let types: [PieceType; 7] = [
        PieceType::Soldier,
        PieceType::Cannon,
        PieceType::Horse,
        PieceType::Chariot,
        PieceType::Elephant,
        PieceType::Advisor,
        PieceType::General,
    ];
    let mut dead: [Vec<PieceType>; 2] = Default::default();
    for (player_idx, out) in dead.iter_mut().enumerate() {
        for (t, &pt) in types.iter().enumerate() {
            for _ in 0..snap.dead_counts[player_idx][t] {
                out.push(pt);
            }
        }
    }

    let hidden: [Vec<Piece>; 2] = [
        snap.hidden_bag
            .iter()
            .filter(|p| p.player == Player::Red)
            .copied()
            .collect(),
        snap.hidden_bag
            .iter()
            .filter(|p| p.player == Player::Black)
            .copied()
            .collect(),
    ];

    BoardProjection {
        cells,
        dead,
        hidden,
        current_player: snap.current_player,
        move_counter: snap.move_counter,
        total_step_counter: snap.total_step_counter,
    }
}

// 终局原因

/// 终局原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverReason {
    /// 胜负分出（将死 / 困毙）
    Checkmate,
    /// 无吃子步数达上限判和
    Draw,
    /// 总步数截断
    MaxSteps,
}

impl OverReason {
    pub fn as_str(self) -> &'static str {
        match self {
            OverReason::Checkmate => "checkmate",
            OverReason::Draw => "draw",
            OverReason::MaxSteps => "max_steps",
        }
    }
}

/// 终局原因推导（胜负分出优先于截断）；未结束时返回 `None`。
pub fn over_reason(over: bool, truncated: bool, winner: Option<i32>) -> Option<OverReason> {
    if !over {
        return None;
    }
    if matches!(winner, Some(1) | Some(-1)) {
        Some(OverReason::Checkmate)
    } else if truncated {
        Some(OverReason::MaxSteps)
    } else {
        Some(OverReason::Draw)
    }
}

// 会话级中立视图（GameView）：gui / server DTO 的公共组装层

/// 带行列坐标的格位视图。
#[derive(Debug, Clone, Copy)]
pub struct LocatedCell {
    /// 格位索引（0..rows*cols，行优先）
    pub index: usize,
    pub row: usize,
    pub col: usize,
    pub state: CellState,
    /// 仅 `Revealed` 时有值（暗子身份不泄露）
    pub piece: Option<Piece>,
}

/// 合法动作的中性视图（坐标经 `action_lookup_tables` 公开 API 查询）。
#[derive(Debug, Clone, Copy)]
pub struct ActionView {
    pub action: usize,
    /// 源格索引（翻棋时 == to）
    pub from: usize,
    /// 目标格索引
    pub to: usize,
    /// 是否机会动作（翻棋 / 吃暗子，结果有随机性）
    pub is_chance: bool,
}

/// 快照标量字段的中性视图。
#[derive(Debug, Clone)]
pub struct ScalarView {
    pub current_player: Player,
    /// 连续无吃子步数
    pub move_count: usize,
    /// 总步数
    pub total_steps: usize,
    pub max_steps: usize,
    /// `[红方, 黑方]` 得分（血量）
    pub scores: [i32; 2],
    pub terminated: bool,
    pub truncated: bool,
    /// 全局视角：Some(1)=红 / Some(-1)=黑 / Some(0)=平局，None=未结束
    pub winner: Option<i32>,
    pub over_reason: Option<OverReason>,
    /// 最近一步的动作索引（开局前为 None）
    pub last_action: Option<usize>,
    /// 当前局面 BFEN 文本（不编码 last_action 与 true_board）
    pub fen: String,
}

/// 会话的完整中立视图：棋盘 / 阵亡 / 隐藏 / 合法动作 / 标量 / 棋谱 / 可悔棋。
/// gui 与 server 的 DTO 均由此薄壳映射，组装逻辑在此单点维护；
/// 字符串格式化差异保留在各侧 DTO 映射处。
#[derive(Debug, Clone)]
pub struct GameView {
    /// 全部格位（行优先，长度 = rows × cols）
    pub cells: Vec<LocatedCell>,
    /// `[红方阵亡, 黑方阵亡]`：按 `dead_counts` 展开为逐枚棋子类型
    pub dead: [Vec<PieceType>; 2],
    /// `[红方未翻, 黑方未翻]`：`hidden_bag` 按归属方过滤
    pub hidden: [Vec<Piece>; 2],
    pub legal_actions: Vec<ActionView>,
    pub scalars: ScalarView,
    /// 决策历史（棋谱）：`revealed`/`captured` 为原始 `Piece`
    pub history: Vec<crate::app::common::HistoryEntry>,
    pub undoable: bool,
}

/// 由会话核心产出完整中立视图。
pub fn game_view(core: &crate::app::session::SnapshotSession) -> GameView {
    let snap = &core.current;
    let cfg = core.variant.config();
    let (terminated, truncated, winner) = core.over_state();
    let over = terminated || truncated;

    let proj = project(snap);
    let cells: Vec<LocatedCell> = proj
        .cells
        .iter()
        .enumerate()
        .map(|(index, cell)| LocatedCell {
            index,
            row: index / cfg.cols,
            col: index % cfg.cols,
            state: cell.state,
            piece: cell.piece,
        })
        .collect();

    let legal_actions: Vec<ActionView> = core
        .legal_actions()
        .into_iter()
        .map(|action| ActionView {
            action,
            from: crate::app::envops::coords_for_action(core.variant, action)
                .and_then(|c| c.first().copied())
                .unwrap_or(action),
            to: crate::app::envops::coords_for_action(core.variant, action)
                .and_then(|c| c.last().copied())
                .unwrap_or(action),
            is_chance: core.is_chance_action(action),
        })
        .collect();

    let scalars = ScalarView {
        current_player: snap.current_player,
        move_count: snap.move_counter as usize,
        total_steps: snap.total_step_counter as usize,
        max_steps: cfg.max_steps_per_episode,
        scores: [snap.scores[0], snap.scores[1]],
        terminated,
        truncated,
        winner,
        over_reason: over_reason(over, truncated, winner),
        last_action: (snap.last_action >= 0).then_some(snap.last_action as usize),
        fen: snap.to_fen(),
    };

    GameView {
        cells,
        dead: proj.dead,
        hidden: proj.hidden,
        legal_actions,
        scalars,
        history: history_entries(&core.history),
        undoable: core.undoable(),
    }
}

// 前端契约状态视图（GameStateView）：gui / 前端 types.ts `GameState` 的单一真源

/// 完整对局状态视图（前端契约形状）：棋盘为短码字符串，
/// 阵亡 / 隐藏列表为棋子类型全名；位板 / 动作掩码 / 翻子概率由环境重建后直出。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GameStateView {
    pub board: Vec<String>,
    /// `"Red"` / `"Black"`
    pub current_player: String,
    /// 连续无吃子步数
    pub move_counter: u32,
    /// 总步数
    pub total_step_counter: u32,
    pub dead_red: Vec<String>,
    pub dead_black: Vec<String>,
    pub hidden_red: Vec<String>,
    pub hidden_black: Vec<String>,
    pub action_masks: Vec<i32>,
    pub reveal_probabilities: Vec<f32>,
    pub bitboards: std::collections::HashMap<String, Vec<bool>>,
    pub hp_red: i32,
    pub hp_black: i32,
    pub variant: String,
    /// 当前局面 BFEN 文本（不编码 last_action 与 true_board）
    pub fen: String,
}

/// 由快照产出前端契约状态视图（状态 DTO 与 MCTS 节点棋盘共用）；
/// 快照由会话 / core 搜索生成，重建失败视为不可达。
pub fn game_state_view(snap: &PositionSnapshot) -> GameStateView {
    use banqi_core::core::env::SnapshotEnv;

    let proj = project(snap);
    let env = DarkChessEnv::from_snapshot(snap).expect("快照由本服务生成，重建不应失败");
    let board: Vec<String> = proj
        .cells
        .iter()
        .map(|c| match c.state {
            CellState::Empty => "Empty".to_string(),
            CellState::Hidden => "Hidden".to_string(),
            CellState::Revealed => c
                .piece
                .expect("revealed 格位必有棋子")
                .short_name()
                .to_string(),
        })
        .collect();

    GameStateView {
        board,
        current_player: crate::app::common::player_label(proj.current_player),
        move_counter: proj.move_counter,
        total_step_counter: proj.total_step_counter,
        dead_red: proj.dead[0].iter().map(|t| format!("{t:?}")).collect(),
        dead_black: proj.dead[1].iter().map(|t| format!("{t:?}")).collect(),
        hidden_red: proj.hidden[0]
            .iter()
            .map(|p| format!("{:?}", p.piece_type))
            .collect(),
        hidden_black: proj.hidden[1]
            .iter()
            .map(|p| format!("{:?}", p.piece_type))
            .collect(),
        action_masks: env.action_masks(),
        reveal_probabilities: env.get_reveal_probabilities().to_vec(),
        bitboards: env.get_bitboards(),
        hp_red: env.get_hp(Player::Red),
        hp_black: env.get_hp(Player::Black),
        variant: snap.variant.as_str().to_string(),
        fen: snap.to_fen(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banqi_core::core::env::{SnapshotEnv, Variant};
    use banqi_core::core::env::DarkChessEnv;

    #[test]
    fn game_view_covers_initial_snapshot() {
        let core = crate::app::session::SnapshotSession::new(Variant::DarkChess4x8);
        let v = game_view(&core);
        assert_eq!(v.cells.len(), 4 * 8);
        // 行列换算：第 index 格 row = index / cols
        assert_eq!((v.cells[0].row, v.cells[0].col), (0, 0));
        assert_eq!((v.cells[9].row, v.cells[9].col), (1, 1));
        assert_eq!(v.scalars.current_player, Player::Red);
        assert_eq!(v.scalars.move_count, 0);
        assert!(v.scalars.over_reason.is_none());
        assert!(v.scalars.last_action.is_none());
        assert!(!v.scalars.fen.is_empty());
        assert!(!v.undoable);
        // 初始局面只有翻棋类合法动作（均为机会动作）
        assert!(!v.legal_actions.is_empty());
        assert!(v.legal_actions.iter().all(|a| a.is_chance));
        assert!(v.history.is_empty());
    }

    #[test]
    fn game_view_tracks_move_and_history() {
        let mut core = crate::app::session::SnapshotSession::new(Variant::DarkChess4x8);
        let action = *core
            .legal_actions()
            .iter()
            .find(|&&a| core.is_chance_action(a))
            .expect("初始局面必有可翻棋动作");
        core.apply_action(action).expect("翻棋应成功");
        let v = game_view(&core);
        assert_eq!(v.scalars.total_steps, 1);
        assert_eq!(v.scalars.last_action, Some(action));
        assert_eq!(v.history.len(), 1);
        assert!(v.history[0].is_flip && v.history[0].revealed.is_some());
        // 行棋方已切换为黑，黑尚未行动 → 不可悔棋
        assert_eq!(v.scalars.current_player, Player::Black);
        assert!(!v.undoable);
    }

    #[test]
    fn projects_initial_snapshot() {
        let env = DarkChessEnv::with_config(Variant::DarkChess4x8.config());
        let proj = project(&env.to_snapshot());
        assert_eq!(proj.cells.len(), 4 * 8);
        // 初始局面：格位非空（暗子或预设翻开的子），且未分胜负故无阵亡子
        assert!(proj
            .cells
            .iter()
            .all(|c| matches!(c.state, CellState::Hidden | CellState::Revealed)));
        assert!(proj.dead[0].is_empty() && proj.dead[1].is_empty());
        // 未翻开（隐藏）的棋子两侧均有，总数 = 32 − 预设翻开的子
        let revealed_count = proj
            .cells
            .iter()
            .filter(|c| c.state == CellState::Revealed)
            .count();
        assert_eq!(proj.hidden[0].len() + proj.hidden[1].len(), 32 - revealed_count);
        assert!(!proj.hidden[0].is_empty() && !proj.hidden[1].is_empty());
        assert_eq!(proj.current_player, Player::Red);
    }

    #[test]
    fn over_reason_precedence() {
        assert_eq!(over_reason(false, false, None), None);
        assert_eq!(over_reason(true, false, Some(1)), Some(OverReason::Checkmate));
        assert_eq!(over_reason(true, false, Some(-1)), Some(OverReason::Checkmate));
        assert_eq!(over_reason(false, true, Some(0)), None, "未结束则无原因");
        assert_eq!(over_reason(true, false, Some(0)), Some(OverReason::Draw));
        assert_eq!(over_reason(true, true, None), Some(OverReason::MaxSteps));
        assert_eq!(over_reason(true, true, Some(1)), Some(OverReason::Checkmate));
    }
}
