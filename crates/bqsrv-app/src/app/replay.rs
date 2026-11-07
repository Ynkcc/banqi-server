use banqi_core::core::env::PositionSnapshot;

use bqsrv_dto::{
    describe_side, termination_label, ArchivedGameDetail, HiddenBagDto, HistoryMoveView,
    ReplayActionDto, ReplayResp, ReplayStepDto,
};

use bq_replay::{ReplayGame, ReplayInput};

use crate::state::ArchivedGame;

pub const SOURCE_SNAPSHOT: &str = bq_replay::SOURCE_SNAPSHOT;
pub const SOURCE_REPLAY: &str = bq_replay::SOURCE_REPLAY;

fn replay_input(game: &ArchivedGame) -> Result<ReplayInput, String> {
    let mut input = bq_record::to_replay_input(&game.record)?;
    if input.initial_fen.is_none() {
        input.initial_fen = bq_record::initial_fen(&game.record).ok();
    }
    if input.final_fen.is_none() {
        match bq_record::final_fen(&game.record) {
            Ok(fen) => input.final_fen = Some(fen),

            Err(e) => {
                tracing::warn!(
                    game = %game.summary.game_id,
                    "终局 FEN 派生失败（疑似数据矛盾）: {e}"
                );
            }
        }
    }
    Ok(input)
}

fn derive_initial_fen(
    record: &bq_record::GameRecord,
    input: &ReplayInput,
) -> Result<String, String> {
    bq_record::initial_fen(record)
        .or_else(|_| bq_replay::snapshot_at_ply(input, 0).map(|s| s.to_fen()))
}

pub fn build_replay(game: &ArchivedGame) -> Result<ReplayResp, String> {
    let input = replay_input(game)?;
    let initial_fen = derive_initial_fen(&game.record, &input)?;
    let g = bq_replay::build_replay(&input)?;
    Ok(to_replay_resp(game, &initial_fen, g))
}

pub fn snapshot_at_ply(game: &ArchivedGame, ply: usize) -> Result<PositionSnapshot, String> {
    let input = replay_input(game)?;
    bq_replay::snapshot_at_ply(&input, ply)
}

pub fn to_detail(game: &ArchivedGame) -> Result<ArchivedGameDetail, String> {
    let input = replay_input(game)?;
    let initial_fen = derive_initial_fen(&game.record, &input)?;

    let final_fen = input
        .final_fen
        .clone()
        .or_else(|| match bq_record::final_fen(&game.record) {
            Ok(fen) => Some(fen),
            Err(e) => {
                tracing::warn!(
                    game = %game.summary.game_id,
                    "终局 FEN 派生失败（疑似数据矛盾）: {e}"
                );
                None
            }
        })
        .unwrap_or_else(|| input.initial_fen.clone().unwrap_or_default());
    let actions: Vec<ReplayActionDto> = input
        .moves
        .iter()
        .map(|m| ReplayActionDto {
            action: m.action,
            revealed: m
                .revealed
                .as_deref()
                .and_then(bq_replay::short_name_to_piece)
                .map(|p| banqi_core::core::env::piece_letter(p.piece_type, p.player)),
        })
        .collect();
    let moves: Vec<HistoryMoveView> = input
        .moves
        .iter()
        .enumerate()
        .map(|(i, m)| HistoryMoveView {
            ply: i + 1,
            player: m.player.clone(),
            mover: None,
            action: m.action,
            from: m.from,
            to: m.to,
            is_flip: m.is_flip,
            is_chance: m.is_chance,
            is_capture: m.is_capture,
            revealed: m.revealed.clone(),
            captured: m.captured.clone(),
        })
        .collect();
    let play = game.record.play.as_ref().ok_or("缺少 play 分组")?;
    let snapshots: Vec<String> = play
        .snapshots
        .iter()
        .map(|b| bqsrv_dto::hex_encode(b))
        .collect();
    let final_snapshot = input
        .final_snapshot
        .as_ref()
        .map(|b| bqsrv_dto::hex_encode(b));
    Ok(ArchivedGameDetail {
        summary: game.summary.clone(),
        initial_fen,
        final_fen,
        actions,
        moves,
        snapshots,
        final_snapshot,
        step_stats: game.step_stats.clone(),
    })
}

fn to_replay_resp(game: &ArchivedGame, initial_fen: &str, g: ReplayGame) -> ReplayResp {
    let moves = g
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
            hidden_bag: HiddenBagDto {
                red: s.hidden_bag.red,
                black: s.hidden_bag.black,
            },
            board_source: s.board_source,
            stats: game.step_stats.get(i).cloned().flatten(),
        })
        .collect();

    ReplayResp {
        game_id: game.summary.game_id.clone(),
        variant: game.summary.variant.clone(),
        termination: termination_label(game.summary.over_reason.as_deref()),
        winner: game.summary.winner.clone(),
        side_red: describe_side(&game.summary.slots.red),
        side_black: describe_side(&game.summary.slots.black),
        initial_fen: initial_fen.to_string(),
        initial_board: g.initial_board,
        initial_shown: g.initial_shown,
        initial_scores: g.initial_scores,
        initial_hidden_bag: HiddenBagDto {
            red: g.initial_hidden_bag.red,
            black: g.initial_hidden_bag.black,
        },
        final_board: g.final_board,
        final_shown: g.final_shown,
        final_scores: g.final_scores,
        final_hidden_bag: HiddenBagDto {
            red: g.final_hidden_bag.red,
            black: g.final_hidden_bag.black,
        },
        board_source: g.board_source,
        moves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::player::Slots;
    use crate::state::GameSession;

    fn play_session(steps: usize) -> GameSession {
        let mut s = GameSession::new(
            banqi_core::core::env::Variant::DarkChess4x8,
            Slots::all_human(),
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

    fn archived(steps: usize) -> ArchivedGame {
        let s = play_session(steps);
        s.archived_game(uuid::Uuid::new_v4(), 1_000)
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
        let game = archived(6);
        let detail = to_detail(&game).expect("派生 detail 应成功");
        let resp = build_replay(&game).expect("快照路径应成功");
        assert_eq!(resp.board_source, SOURCE_SNAPSHOT);
        assert_eq!(resp.moves.len(), detail.moves.len());
        assert_eq!(resp.moves.len(), 6);
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
    fn archive_json_roundtrip_preserves_replay() {
        let game = archived(5);
        let text = serde_json::to_string(&game).expect("序列化应成功");
        assert!(text.contains("\"record\""), "归档 JSON 应含 record 字段");
        assert!(
            text.contains("\"terminationReason\""),
            "record 分组应为 camelCase"
        );
        let loaded: ArchivedGame = serde_json::from_str(&text).expect("反序列化应成功");
        let resp = build_replay(&loaded).expect("加载归档回放应成功");
        assert_eq!(resp.board_source, SOURCE_SNAPSHOT);
        assert_eq!(resp.moves.len(), 5);

        let with_stats = resp.moves.iter().filter(|m| m.stats.is_some()).count();
        assert!(with_stats > 0, "应捕获到策略步诊断");
    }

    #[test]
    fn empty_moves_archive_still_replays() {
        let game = archived(0);
        assert_eq!(game.record.steps(), 0);
        let resp = build_replay(&game).expect("空局回放不应失败");
        assert!(resp.moves.is_empty());
        assert_eq!(resp.final_board.len(), 32);
        let detail = to_detail(&game).expect("空局派生 detail 应成功");
        assert!(detail.moves.is_empty());
        assert!(detail.initial_fen.contains(' '), "应回退变体默认 FEN");
    }

    #[test]
    fn to_detail_matches_replay_output() {
        let game = archived(5);
        let detail = to_detail(&game).expect("派生 detail 应成功");
        let resp = build_replay(&game).expect("回放应成功");
        assert_eq!(detail.initial_fen, resp.initial_fen);
        assert_eq!(detail.moves.len(), resp.moves.len());
        assert_eq!(detail.snapshots.len(), 5);
        assert!(detail.final_snapshot.is_some(), "终局快照应由重放派生");
        assert!(detail.moves.iter().all(|m| m.mover.is_none()));
        for (m, a) in detail.moves.iter().zip(&detail.actions) {
            assert_eq!(m.action, a.action);
        }

        assert_eq!(detail.summary.game_id, game.summary.game_id);
        assert_eq!(
            game.record.source(),
            Some(bq_record::RecordSource::SourceServer)
        );
        assert_eq!(
            game.record.identity.as_ref().unwrap().game_id.as_deref(),
            Some(detail.summary.game_id.as_str())
        );
    }

    #[test]
    fn record_outcome_mirrors_summary() {
        let game = archived(4);
        let outcome = game.record.outcome.as_ref().expect("outcome 应存在");
        let expected = crate::state::from_server_over_reason(game.summary.over_reason.as_deref());
        assert_eq!(outcome.termination_reason, expected);
        let timing = game.record.timing.as_ref().expect("timing 应存在");
        assert_eq!(timing.created_at, game.summary.created_at as i64);
        assert_eq!(timing.finished_at, game.summary.finished_at as i64);
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
    fn dto_detail_json_roundtrip_keeps_shape() {
        let detail = to_detail(&archived(3)).expect("派生 detail 应成功");
        let text = serde_json::to_string(&detail).expect("序列化应成功");
        let back: ArchivedGameDetail = serde_json::from_str(&text).expect("反序列化应成功");
        assert_eq!(back.moves.len(), detail.moves.len());
        assert_eq!(back.initial_fen, detail.initial_fen);
    }
}
