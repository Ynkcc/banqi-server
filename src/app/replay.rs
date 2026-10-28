use banqi_core::core::env::types::{Piece, PieceType, Player, Slot};
use banqi_core::core::env::{PositionSnapshot, SnapshotEnv, Variant};

use crate::dto::{
    ArchivedGameDetail, HiddenBagDto, HistoryMoveView, ReplayResp, ReplayStepDto, describe_side,
    hex_decode, termination_label,
};

pub const SOURCE_SNAPSHOT: &str = "snapshot";
pub const SOURCE_REPLAY: &str = "replay";

pub fn build_replay(detail: &ArchivedGameDetail) -> Result<ReplayResp, String> {
    let variant = crate::config::parse_variant(&detail.summary.variant)
        .ok_or_else(|| format!("未知变体: {}", detail.summary.variant))?;
    let positions = {
        let cfg = variant.config();
        cfg.rows * cfg.cols
    };

    let snaps: Option<Vec<PositionSnapshot>> =
        if detail.snapshots.len() == detail.moves.len() && !detail.moves.is_empty() {
            detail
                .snapshots
                .iter()
                .map(|hex| {
                    let bytes = hex_decode(hex).ok_or("快照 hex 非法")?;
                    PositionSnapshot::decode(&bytes)
                        .map_err(|e| format!("快照解码失败: {e}"))
                        .map(|s| {
                            debug_assert_eq!(s.slots.len(), positions, "快照槽位数应与变体一致");
                            s
                        })
                })
                .collect::<Result<Vec<_>, String>>()
                .ok()
        } else {
            None
        };
    let board_source = if snaps.is_some() {
        SOURCE_SNAPSHOT
    } else {
        SOURCE_REPLAY
    };

    let replay_snaps = if snaps.is_none() && !detail.moves.is_empty() {
        Some(replay_snapshots(variant, detail)?)
    } else {
        None
    };
    let per_ply = |i: usize| -> PositionSnapshot {
        if let Some(v) = &snaps {
            v[i].clone()
        } else if let Some(v) = &replay_snaps {
            v[i].clone()
        } else {
            PositionSnapshot::from_fen(detail.initial_fen.trim())
                .unwrap_or_else(|_| variant_initial_snapshot(variant))
        }
    };

    let mut moves = Vec::with_capacity(detail.moves.len());
    for (i, m) in detail.moves.iter().enumerate() {
        let kind = step_kind(m);
        let snap = per_ply(i);
        let (board, revealed_at, hidden_bag) = god_view(&snap);
        moves.push(ReplayStepDto {
            ply: i + 1,
            player: m.player.clone(),
            action: m.action,
            from: m.from,
            to: m.to,
            kind: kind.to_string(),
            is_chance: m.is_chance,
            revealed: m.revealed.clone(),
            captured: m.captured.clone(),
            move_counter: post_move_counter(kind, snap.move_counter),
            scores: snap.scores,
            board,
            revealed_at,
            hidden_bag,
            board_source: board_source.to_string(),
            stats: detail.step_stats.get(i).cloned().flatten(),
        });
    }

    let initial_snap = per_ply(0);
    let (initial_board, initial_shown, initial_hidden_bag) = god_view(&initial_snap);
    let initial_scores = initial_snap.scores;
    let final_snap = final_snapshot(detail, &per_ply(detail.moves.len().saturating_sub(1)));
    let (final_board, final_shown, final_hidden_bag) = god_view(&final_snap);
    let final_scores = final_snap.scores;

    Ok(ReplayResp {
        game_id: detail.summary.game_id.clone(),
        variant: detail.summary.variant.clone(),
        termination: termination_label(detail.summary.over_reason.as_deref()),
        winner: detail.summary.winner.clone(),
        side_red: describe_side(&detail.summary.slots.red),
        side_black: describe_side(&detail.summary.slots.black),
        initial_fen: detail.initial_fen.clone(),
        initial_board,
        initial_shown,
        initial_scores,
        initial_hidden_bag,
        final_board,
        final_shown,
        final_scores,
        final_hidden_bag,
        board_source: board_source.to_string(),
        moves,
    })
}

pub fn snapshot_at_ply(
    detail: &ArchivedGameDetail,
    ply: usize,
) -> Result<PositionSnapshot, String> {
    let variant = crate::config::parse_variant(&detail.summary.variant)
        .ok_or_else(|| format!("未知变体: {}", detail.summary.variant))?;
    let len = detail.moves.len();
    if len == 0 || ply == 0 {
        return PositionSnapshot::from_fen(detail.initial_fen.trim())
            .map_err(|e| format!("归档初始局面解析失败: {e}"));
    }
    if ply >= len {
        return Ok(final_snapshot(
            detail,
            &PositionSnapshot::from_fen(detail.initial_fen.trim())
                .unwrap_or_else(|_| variant_initial_snapshot(variant)),
        ));
    }

    let pre = if detail.snapshots.len() == len {
        let hex = detail
            .snapshots
            .get(ply - 1)
            .ok_or_else(|| format!("快照缺失: ply={ply}"))?;
        let bytes = hex_decode(hex).ok_or("快照 hex 非法")?;
        PositionSnapshot::decode(&bytes).map_err(|e| format!("快照解码失败: {e}"))?
    } else {
        replay_snapshots(variant, detail)?.swap_remove(ply - 1)
    };
    Ok(pre)
}

fn replay_snapshots(
    variant: Variant,
    detail: &ArchivedGameDetail,
) -> Result<Vec<PositionSnapshot>, String> {
    let mut snap = PositionSnapshot::from_fen(detail.initial_fen.trim())
        .map_err(|e| format!("归档初始局面解析失败: {e}"))?;
    let mut out = Vec::with_capacity(detail.moves.len());
    for (i, m) in detail.moves.iter().enumerate() {
        out.push(snap.clone());

        let reveal = m.revealed.as_deref().and_then(short_name_to_piece);
        snap = crate::with_env!(variant, &snap, env => {
            env.step_with_reveal(m.action, reveal).map(|_| env.to_snapshot())
        })
        .and_then(std::convert::identity)
        .map_err(|e| format!("第 {} 步（action={}）动作重放失败: {e}", i + 1, m.action))?;
    }
    Ok(out)
}

fn short_name_to_piece(s: &str) -> Option<Piece> {
    let (p, t) = s.split_once('_')?;
    let player = match p {
        "R" => Player::Red,
        "B" => Player::Black,
        _ => return None,
    };
    let piece_type = match t {
        "Sol" => PieceType::Soldier,
        "Can" => PieceType::Cannon,
        "Hor" => PieceType::Horse,
        "Cha" => PieceType::Chariot,
        "Ele" => PieceType::Elephant,
        "Adv" => PieceType::Advisor,
        "Gen" => PieceType::General,
        _ => return None,
    };
    Some(Piece::new(piece_type, player))
}

fn final_snapshot(detail: &ArchivedGameDetail, fallback: &PositionSnapshot) -> PositionSnapshot {
    if let Some(hex) = &detail.final_snapshot
        && let Some(bytes) = hex_decode(hex)
        && let Ok(snap) = PositionSnapshot::decode(&bytes)
    {
        return snap;
    }
    PositionSnapshot::from_fen(detail.final_fen.trim()).unwrap_or_else(|_| fallback.clone())
}

fn variant_initial_snapshot(variant: Variant) -> PositionSnapshot {
    banqi_core::for_variant_env!(variant, E => {
        <E as Default>::default().to_snapshot()
    })
}

fn step_kind(m: &HistoryMoveView) -> &'static str {
    if m.is_flip {
        "flip"
    } else if m.is_chance {
        "reveal-attack"
    } else if m.is_capture {
        "capture"
    } else {
        "move"
    }
}

fn post_move_counter(kind: &str, pre: u32) -> u32 {
    if kind == "move" { pre + 1 } else { 0 }
}

fn god_view(snap: &PositionSnapshot) -> (Vec<String>, Vec<bool>, HiddenBagDto) {
    let mut board = Vec::with_capacity(snap.slots.len());
    let mut shown = Vec::with_capacity(snap.slots.len());
    let mut bag = HiddenBagDto::default();
    for p in &snap.hidden_bag {
        count_hidden(&mut bag, p);
    }
    for (sq, slot) in snap.slots.iter().enumerate() {
        match slot {
            Slot::Empty => {
                board.push("Empty".to_string());
                shown.push(true);
            }
            Slot::Hidden => {
                let identity = snap
                    .true_board
                    .as_deref()
                    .and_then(|tb| tb.get(sq))
                    .copied();
                match identity {
                    Some(p) => board.push(p.short_name()),
                    None => board.push("Hidden".to_string()),
                }
                shown.push(false);
            }
            Slot::Revealed(p) => {
                board.push(p.short_name());
                shown.push(true);
            }
        }
    }
    (board, shown, bag)
}

fn count_hidden(bag: &mut HiddenBagDto, p: &Piece) {
    let arr = if p.player == Player::Red {
        &mut bag.red
    } else {
        &mut bag.black
    };
    let idx = p.piece_type as usize;
    if idx < arr.len() {
        arr[idx] = arr[idx].saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GameSession;

    fn play_session(steps: usize) -> GameSession {
        let mut s = GameSession::new(
            Variant::DarkChess4x8,
            crate::app::player::Slots::all_human(),
        );
        for _ in 0..steps {
            if s.over_state().0 || s.over_state().1 {
                break;
            }
            let action = *s.legal_actions().first().expect("未终局必有合法动作");
            s.apply_action(action).expect("合法动作应成功");

            if s.core.history.records().len() % 2 == 0 {
                s.capture_step_stats(crate::dto::StepStatsDto {
                    mcts: Some(crate::dto::MctsStatsDto {
                        reused: false,
                        budget: 200,
                        actual_sims: 120,
                        max_depth: 6,
                        nodes: 3000,
                        elapsed_ms: 40,
                    }),
                    nnue: None,
                });
            }
        }
        s
    }

    fn archived(steps: usize) -> ArchivedGameDetail {
        let s = play_session(steps);
        s.archived_detail(uuid::Uuid::new_v4(), 1_000)
    }

    #[test]
    fn snapshot_path_rebuilds_god_view() {
        let detail = archived(6);
        let resp = build_replay(&detail).expect("快照路径应成功");
        assert_eq!(resp.board_source, SOURCE_SNAPSHOT);
        assert_eq!(resp.moves.len(), detail.moves.len());
        for (i, m) in resp.moves.iter().enumerate() {
            assert_eq!(m.ply, i + 1);
            assert_eq!(m.board.len(), 32, "4x8 共 32 格");
            assert_eq!(m.revealed_at.len(), 32);
            assert_eq!(m.board_source, SOURCE_SNAPSHOT);
            assert_eq!(m.kind, step_kind(&detail.moves[i]));
        }

        assert_eq!(resp.final_board.len(), 32);
        assert!(resp.side_red.starts_with("人类玩家"));
        assert!(resp.side_black.starts_with("人类玩家"));

        let hidden = |m: &ReplayStepDto| m.revealed_at.iter().filter(|s| !**s).count();
        for w in resp.moves.windows(2) {
            assert!(hidden(&w[1]) <= hidden(&w[0]), "暗子数不应增加");
        }
    }

    #[test]
    fn replay_fallback_when_snapshots_stripped() {
        let mut detail = archived(4);
        detail.snapshots.clear();
        detail.final_snapshot = None;
        let resp = build_replay(&detail).expect("回退路径应成功");
        assert_eq!(resp.board_source, SOURCE_REPLAY);
        assert_eq!(resp.moves.len(), detail.moves.len());

        let detail_full = archived(4);
        let mut stripped = detail_full.clone();
        stripped.snapshots.clear();
        stripped.final_snapshot = None;
        let resp2 = build_replay(&stripped).expect("回退路径应成功");
        let full = build_replay(&detail_full).expect("快照路径应成功");
        assert_eq!(
            resp2.final_board, full.final_board,
            "回退终局应与快照路径一致"
        );
    }

    #[test]
    fn archive_json_roundtrip_preserves_replay() {
        let detail = archived(5);
        let text = serde_json::to_string(&detail).expect("序列化应成功");
        let loaded: ArchivedGameDetail = serde_json::from_str(&text).expect("反序列化应成功");
        let resp = build_replay(&loaded).expect("加载归档回放应成功");
        assert_eq!(resp.board_source, SOURCE_SNAPSHOT);
        assert_eq!(resp.moves.len(), 5);

        let with_stats = resp.moves.iter().filter(|m| m.stats.is_some()).count();
        assert!(with_stats > 0, "应捕获到策略步诊断");
    }

    #[test]
    fn empty_moves_archive_still_replays() {
        let mut detail = archived(1);
        detail.moves.clear();
        detail.snapshots.clear();
        detail.final_snapshot = None;
        detail.step_stats.clear();
        let resp = build_replay(&detail).expect("空局回放不应失败");
        assert!(resp.moves.is_empty());
    }

    #[test]
    fn session_snapshots_align_with_records() {
        let s = play_session(8);
        assert_eq!(
            s.core.history.snapshots().len(),
            s.core.history.records().len(),
            "快照与步记录应一一对应（含悔棋路径）"
        );
    }
}
