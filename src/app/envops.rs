use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::error::EnvError;
use banqi_core::core::env::{GameEnv, PositionSnapshot, SnapshotEnv, Variant};
#[macro_export]
macro_rules! with_env {
    ($variant:expr, $snap:expr, $e:ident => $body:expr) => {
        banqi_core::for_variant_env!($variant, E => {
            <E as banqi_core::core::env::SnapshotEnv>::from_snapshot($snap).map(|$e| {
                #[allow(unused_mut)]
                let mut $e = $e;
                $body
            })
        })
    };
}
pub fn initial_snapshot(variant: Variant) -> PositionSnapshot {
    banqi_core::for_variant_env!(variant, E => {
        <E as Default>::default().to_snapshot()
    })
}
pub fn initial_snapshot_with_scenario(
    variant: Variant,
    scenario: Option<&banqi_core::core::env::OpeningScenario>,
) -> Result<PositionSnapshot, EnvError> {
    use banqi_core::core::env::error::EnvError;
    let Some(s) = scenario else {
        return Ok(initial_snapshot(variant));
    };
    if !s.supports_variant(variant) {
        return Err(EnvError::InvalidScenario {
            context: "该变体不支持此开局场景",
        });
    }
    let env =
        banqi_core::core::env::DarkChessEnv::with_config_and_scenario(variant.config(), Some(*s))?;
    Ok(env.to_snapshot())
}
pub fn legal_actions<E: GameEnv>(env: &E) -> Vec<usize> {
    let n = env.action_space_size();
    let mut masks = vec![0i32; n];
    env.action_masks_into(&mut masks);
    masks
        .iter()
        .enumerate()
        .filter(|(_, m)| **m != 0)
        .map(|(a, _)| a)
        .collect()
}
#[derive(Clone, Copy, Debug)]
pub struct StepOutcome {
    pub terminated: bool,
    pub truncated: bool,
    pub winner: Option<i32>,
}
impl StepOutcome {
    pub fn is_over(&self) -> bool {
        self.terminated || self.truncated
    }
}
pub fn step<E: GameEnv>(env: &mut E, action: usize) -> Result<StepOutcome, EnvError> {
    let (_, terminated, truncated, winner) = env.step(action)?;
    Ok(StepOutcome {
        terminated,
        truncated,
        winner,
    })
}
pub fn game_over<E: GameEnv>(env: &E) -> (bool, bool, Option<i32>) {
    env.check_game_over_conditions()
}
pub fn coords_for_action(variant: Variant, action: usize) -> Option<Vec<usize>> {
    action_lookup_tables(&variant.config())
        .action_to_coords
        .get(action)
        .cloned()
}

pub fn replay_plies(
    variant: Variant,
    initial: &PositionSnapshot,
    moves: &[(usize, Option<banqi_core::core::env::Piece>)],
) -> Result<Vec<crate::dto::ReplayPly>, String> {
    use crate::app::common::player_label;
    let mut snap = initial.clone();
    let mut plies = vec![crate::dto::ReplayPly {
        ply: 0,
        player: player_label(snap.current_player),
        fen: snap.to_fen(),
        last_action: None,
    }];
    for (i, &(action, reveal)) in moves.iter().enumerate() {
        snap = crate::with_env!(variant, &snap, env => {
            env.step_with_reveal(action, reveal)
                .map(|_| env.to_snapshot())
        })
        .and_then(std::convert::identity)
        .map_err(|e| format!("第 {} 步（action={action}）推演失败: {e}", i + 1))?;
        plies.push(crate::dto::ReplayPly {
            ply: i + 1,
            player: player_label(snap.current_player),
            fen: snap.to_fen(),
            last_action: Some(action),
        });
    }
    Ok(plies)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::session::SnapshotSession;
    fn move_pairs(live: &SnapshotSession) -> Vec<(usize, Option<banqi_core::core::env::Piece>)> {
        live.history
            .records()
            .iter()
            .map(|r| (r.action, r.revealed))
            .collect()
    }
    #[test]
    fn replay_plies_roundtrip_matches_live_session() {
        let variant = Variant::DarkChess4x8;
        let mut live = SnapshotSession::new(variant);
        let initial = live.current.clone();
        for _ in 0..8 {
            let action = *live.legal_actions().first().expect("总有合法动作");
            live.apply_action(action).expect("走子应成功");
        }
        let moves = move_pairs(&live);
        let plies = replay_plies(variant, &initial, &moves).expect("推演应成功");
        assert_eq!(plies.len(), moves.len() + 1);
        assert_eq!(plies[0].fen, initial.to_fen());
        assert!(plies[0].last_action.is_none());
        assert_eq!(plies[1].last_action, Some(moves[0].0));
        assert_eq!(
            plies.last().expect("非空").fen,
            live.current.to_fen(),
            "推演终局局面应与实况一致（含暗子袋）"
        );
    }
    #[test]
    fn replay_plies_rejects_illegal_action() {
        let variant = Variant::DarkChess4x8;
        let mut live = SnapshotSession::new(variant);
        let initial = live.current.clone();
        let action = *live.legal_actions().first().expect("总有合法动作");
        let illegal = (0..)
            .find(|&a| !live.legal_actions().contains(&a))
            .expect("动作空间大于合法集合");
        live.apply_action(action).expect("走子应成功");
        let moves = vec![(action, None), (illegal, None)];
        let err = replay_plies(variant, &initial, &moves).expect_err("非法动作应报错");
        assert!(err.contains("推演失败"));
    }
}
