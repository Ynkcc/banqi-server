use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::error::EnvError;
use banqi_core::core::env::{DarkChessEnv, GameEnv, PositionSnapshot, SnapshotEnv, Variant};
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
pub use banqi_core::core::env::outcome::StepOutcome;
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
    env.legal_actions()
}
pub fn step<E: GameEnv>(env: &mut E, action: usize) -> Result<StepOutcome, EnvError> {
    banqi_core::core::env::outcome::step(env, action)
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

pub fn is_flip_action(variant: Variant, action: usize) -> bool {
    action < variant.config().reveal_actions_count
}

pub fn action_for_coords(
    variant: Variant,
    snap: &PositionSnapshot,
    from: usize,
    to: usize,
) -> Result<Option<usize>, String> {
    let cfg = variant.config();
    let total = cfg.rows * cfg.cols;
    if from >= total || to >= total {
        return Err(format!(
            "坐标越界: from={from} to={to}（变体 {} 共 {total} 格）",
            variant.as_str()
        ));
    }
    if from == to {
        let tables = action_lookup_tables(&cfg);
        let a = tables.single_to_action[from];
        return Ok((a >= 0).then_some(a as usize));
    }
    use banqi_core::core::env::SnapshotEnv;
    let env = DarkChessEnv::from_snapshot(snap).map_err(|e| format!("重建环境失败: {e}"))?;
    Ok(env.get_action_for_coords(&[from, to]))
}
