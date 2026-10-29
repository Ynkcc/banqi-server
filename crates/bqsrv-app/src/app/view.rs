use crate::app::common::history_entries;
pub use banqi_core::core::env::outcome::{OverReason, over_reason};
pub use banqi_core::core::env::projection::{BoardCell, BoardProjection, CellState, project};
use banqi_core::core::env::types::{Piece, PieceType, Player};
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot};
pub use bqsrv_dto::{ActionView, GameStateView};
#[derive(Debug, Clone, Copy)]
pub struct LocatedCell {
    pub index: usize,
    pub row: usize,
    pub col: usize,
    pub state: CellState,
    pub piece: Option<Piece>,
}
#[derive(Debug, Clone)]
pub struct ScalarView {
    pub current_player: Player,
    pub move_count: usize,
    pub total_steps: usize,
    pub max_steps: usize,
    pub scores: [i32; 2],
    pub terminated: bool,
    pub truncated: bool,
    pub winner: Option<i32>,
    pub over_reason: Option<OverReason>,
    pub last_action: Option<usize>,
    pub fen: String,
}
#[derive(Debug, Clone)]
pub struct GameView {
    pub cells: Vec<LocatedCell>,
    pub dead: [Vec<PieceType>; 2],
    pub hidden: [Vec<Piece>; 2],
    pub legal_actions: Vec<ActionView>,
    pub scalars: ScalarView,
    pub history: Vec<crate::app::common::HistoryEntry>,
    pub undoable: bool,
}

pub fn derive_action_view(
    variant: banqi_core::core::env::Variant,
    action: usize,
    is_chance: bool,
) -> ActionView {
    let coords = crate::app::envops::coords_for_action(variant, action).unwrap_or_default();
    ActionView {
        action,
        from: coords.first().copied().unwrap_or(action),
        to: coords.last().copied().unwrap_or(action),
        is_chance,
    }
}
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
        .map(|action| derive_action_view(core.variant, action, core.is_chance_action(action)))
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
    use banqi_core::core::env::DarkChessEnv;
    use banqi_core::core::env::{SnapshotEnv, Variant};
    #[test]
    fn game_view_covers_initial_snapshot() {
        let core = crate::app::session::SnapshotSession::new(Variant::DarkChess4x8);
        let v = game_view(&core);
        assert_eq!(v.cells.len(), 4 * 8);
        assert_eq!((v.cells[0].row, v.cells[0].col), (0, 0));
        assert_eq!((v.cells[9].row, v.cells[9].col), (1, 1));
        assert_eq!(v.scalars.current_player, Player::Red);
        assert_eq!(v.scalars.move_count, 0);
        assert!(v.scalars.over_reason.is_none());
        assert!(v.scalars.last_action.is_none());
        assert!(!v.scalars.fen.is_empty());
        assert!(!v.undoable);
        assert!(!v.legal_actions.is_empty());
        assert!(v.legal_actions.iter().all(|a| {
            a.is_chance
                || v.cells[a.from].state == CellState::Revealed
                    && v.cells[a.from].piece.map(|p| p.player) == Some(Player::Red)
        }));
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
        assert_eq!(v.scalars.current_player, Player::Black);
        assert!(!v.undoable);
    }
    #[test]
    fn projects_initial_snapshot() {
        let env = DarkChessEnv::with_config(Variant::DarkChess4x8.config());
        let proj = project(&env.to_snapshot());
        assert_eq!(proj.cells.len(), 4 * 8);
        assert!(
            proj.cells
                .iter()
                .all(|c| matches!(c.state, CellState::Hidden | CellState::Revealed))
        );
        assert!(proj.dead[0].is_empty() && proj.dead[1].is_empty());
        let revealed_count = proj
            .cells
            .iter()
            .filter(|c| c.state == CellState::Revealed)
            .count();
        assert_eq!(
            proj.hidden[0].len() + proj.hidden[1].len(),
            32 - revealed_count
        );
        assert!(!proj.hidden[0].is_empty() && !proj.hidden[1].is_empty());
        assert_eq!(proj.current_player, Player::Red);
    }
    #[test]
    fn over_reason_precedence() {
        assert_eq!(over_reason(false, false, None), None);
        assert_eq!(
            over_reason(true, false, Some(1)),
            Some(OverReason::Checkmate)
        );
        assert_eq!(
            over_reason(true, false, Some(-1)),
            Some(OverReason::Checkmate)
        );
        assert_eq!(over_reason(false, true, Some(0)), None, "未结束则无原因");
        assert_eq!(over_reason(true, false, Some(0)), Some(OverReason::Draw));
        assert_eq!(over_reason(true, true, None), Some(OverReason::MaxSteps));
        assert_eq!(
            over_reason(true, true, Some(1)),
            Some(OverReason::Checkmate)
        );
    }
}
