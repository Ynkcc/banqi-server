use banqi_core::core::env::MoveHistory;
use banqi_core::core::env::types::{Piece, Player};
pub use bqsrv_dto::player_label;
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub ply: usize,
    pub player: Player,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_flip: bool,
    pub is_chance: bool,
    pub is_capture: bool,
    pub revealed: Option<Piece>,
    pub captured: Option<Piece>,
}
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
