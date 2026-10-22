//! 公共辅助（共享）：player 标签与棋谱条目映射。
//!
//! 由 banqi-gui/src/state.rs（`player_label` + `extract_state_with_undo`）
//! 与 banqi-server/src/state.rs（`view()` 的棋谱映射段）收敛而来：
//! `MoveHistory`/`MoveRecord` → 历史视图的公共标量字段在此统一产出，
//! `revealed`/`captured` 的格式化（gui 用 `short_name()`，server 用
//! 小写英文 `PieceView`）保留在各侧 DTO 映射处。

use banqi_core::core::env::types::{Piece, Player};
use banqi_core::core::env::MoveHistory;

/// 行棋方标签（"Red" / "Black"，gui 前端契约）。
pub fn player_label(p: Player) -> String {
    match p {
        Player::Red => "Red".to_string(),
        Player::Black => "Black".to_string(),
    }
}

/// 棋谱条目的公共标量字段；`revealed`/`captured` 保留原始 `Piece`，
/// 字符串格式化由各侧 DTO 映射决定。
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// 手数（1 起，对局内单调递增）
    pub ply: usize,
    pub player: Player,
    pub action: usize,
    /// 源格（翻棋时 == to）
    pub from: usize,
    pub to: usize,
    /// 纯翻棋
    pub is_flip: bool,
    /// 机会动作（翻棋 / 吃暗子，结果有随机性）
    pub is_chance: bool,
    /// 吃明子（目标是对方已翻开的明子）
    pub is_capture: bool,
    /// 机会动作实际翻出的棋子（翻棋 = 翻出的子；吃暗子含炮打暗子 = 打出的子）
    pub revealed: Option<Piece>,
    /// 被消灭的棋子（吃明子的被吃子；吃暗子时 == revealed）
    pub captured: Option<Piece>,
}

/// 由走子历史产出棋谱条目（`MoveHistory::records()` 的 ply 编号 + 公共字段投影）。
pub fn history_entries(history: &MoveHistory) -> Vec<HistoryEntry> {
    history
        .records()
        .iter()
        .enumerate()
        .map(|(i, rec)| HistoryEntry {
            ply: i + 1,
            player: rec.player,
            action: rec.action,
            from: rec.from,
            to: rec.to,
            is_flip: rec.is_flip,
            is_chance: rec.is_chance,
            is_capture: rec.is_capture,
            revealed: rec.revealed,
            captured: rec.captured,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_labels() {
        assert_eq!(player_label(Player::Red), "Red");
        assert_eq!(player_label(Player::Black), "Black");
    }

    #[test]
    fn empty_history_yields_no_entries() {
        assert!(history_entries(&MoveHistory::new()).is_empty());
    }
}
