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
