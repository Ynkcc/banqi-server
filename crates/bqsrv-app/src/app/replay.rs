use banqi_core::core::env::{PositionSnapshot, Variant};

use bqsrv_dto::{
    describe_side, hex_decode, termination_label, ArchivedGameDetail, ReplayResp, ReplayStepDto,
};

use bq_replay::{ReplayGame, ReplayInput, ReplayMove};

pub const SOURCE_SNAPSHOT: &str = bq_replay::SOURCE_SNAPSHOT;
pub const SOURCE_REPLAY: &str = bq_replay::SOURCE_REPLAY;

pub fn build_replay(detail: &ArchivedGameDetail) -> Result<ReplayResp, String> {
    let variant = bqsrv_config::parse_variant(&detail.summary.variant)
        .ok_or_else(|| format!("未知变体: {}", detail.summary.variant))?;
    let input = to_replay_input(variant, detail);
    let game = bq_replay::build_replay(&input)?;
    Ok(to_replay_resp(detail, game))
}

pub fn snapshot_at_ply(
    detail: &ArchivedGameDetail,
    ply: usize,
) -> Result<PositionSnapshot, String> {
    let variant = bqsrv_config::parse_variant(&detail.summary.variant)
        .ok_or_else(|| format!("未知变体: {}", detail.summary.variant))?;
    let input = to_replay_input(variant, detail);
    bq_replay::snapshot_at_ply(&input, ply)
}

fn to_replay_input(variant: Variant, detail: &ArchivedGameDetail) -> ReplayInput {
    let snapshots = if detail.snapshots.len() == detail.moves.len() && !detail.moves.is_empty() {
        detail
            .snapshots
            .iter()
            .map(|hex| hex_decode(hex))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let moves = detail.moves.iter().map(|m| ReplayMove {
        player: m.player.clone(),
        action: m.action,
        from: m.from,
        to: m.to,
        is_flip: m.is_flip,
        is_chance: m.is_chance,
        is_capture: m.is_capture,
        revealed: m.revealed.clone(),
        captured: m.captured.clone(),
    });
    ReplayInput {
        variant,
        initial_fen: Some(detail.initial_fen.clone()),
        initial_layout: None,
        snapshots,
        final_snapshot: detail.final_snapshot.as_deref().and_then(hex_decode),
        final_fen: Some(detail.final_fen.clone()),
        moves: moves.collect(),
    }
}

fn to_replay_resp(detail: &ArchivedGameDetail, game: ReplayGame) -> ReplayResp {
    let moves = game
        .moves
        .into_iter()
        .enumerate()
        .map(|(i, s)| ReplayStepDto {
            ply: s.ply,
            player: s.player,
            action: s.action,
            from: s.from,
            to: s.to,
            kind: s.kind.as_str().to_string(),
            is_chance: s.is_chance,
            revealed: s.revealed,
            captured: s.captured,
            move_counter: s.move_counter,
            scores: s.scores,
            board: s.board,
            revealed_at: s.revealed_at,
            hidden_bag: bqsrv_dto::HiddenBagDto {
                red: s.hidden_bag.red,
                black: s.hidden_bag.black,
            },
            board_source: s.board_source,
            stats: detail.step_stats.get(i).cloned().flatten(),
        })
        .collect();

    ReplayResp {
        game_id: detail.summary.game_id.clone(),
        variant: detail.summary.variant.clone(),
        termination: termination_label(detail.summary.over_reason.as_deref()),
        winner: detail.summary.winner.clone(),
        side_red: describe_side(&detail.summary.slots.red),
        side_black: describe_side(&detail.summary.slots.black),
        initial_fen: detail.initial_fen.clone(),
        initial_board: game.initial_board,
        initial_shown: game.initial_shown,
        initial_scores: game.initial_scores,
        initial_hidden_bag: bqsrv_dto::HiddenBagDto {
            red: game.initial_hidden_bag.red,
            black: game.initial_hidden_bag.black,
        },
        final_board: game.final_board,
        final_shown: game.final_shown,
        final_scores: game.final_scores,
        final_hidden_bag: bqsrv_dto::HiddenBagDto {
            red: game.final_hidden_bag.red,
            black: game.final_hidden_bag.black,
        },
        board_source: game.board_source,
        moves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GameSession;
    use bqsrv_dto::HistoryMoveView;

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

            if s.core.history.records().len().is_multiple_of(2) {
                s.capture_step_stats(bqsrv_dto::StepStatsDto {
                    mcts: Some(bqsrv_dto::MctsStatsDto {
                        reused: false,
                        budget: 200,
                        actual_sims: 120,
                        max_depth: 6,
                        nodes: 3000,
                        elapsed_ms: 40,
                    }),
                    nnue: None,
                    note: None,
                });
            }
        }
        s
    }

    fn archived(steps: usize) -> ArchivedGameDetail {
        let s = play_session(steps);
        s.archived_detail(uuid::Uuid::new_v4(), 1_000)
    }

    fn kind_str(m: &HistoryMoveView) -> &'static str {
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
            assert_eq!(m.kind, kind_str(&detail.moves[i]));
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

    #[test]
    fn dto_mapping_matches_neutral_output() {
        let detail = archived(5);
        let variant = bqsrv_config::parse_variant(&detail.summary.variant).unwrap();
        let input = to_replay_input(variant, &detail);
        let game = bq_replay::build_replay(&input).expect("共享库回放应成功");
        let resp = to_replay_resp(&detail, game.clone());
        assert_eq!(resp.board_source, game.board_source);
        assert_eq!(resp.initial_scores, game.initial_scores);
        for (i, (dto, neutral)) in resp.moves.iter().zip(&game.moves).enumerate() {
            assert_eq!(dto.ply, neutral.ply);
            assert_eq!(dto.board, neutral.board);
            assert_eq!(dto.move_counter, neutral.move_counter);
            assert_eq!(
                serde_json::to_value(&dto.stats).unwrap(),
                serde_json::to_value(detail.step_stats.get(i).cloned().flatten()).unwrap()
            );
        }
    }
}
