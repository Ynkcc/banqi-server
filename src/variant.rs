//! 变体无关的环境操作层。
//!
//! 设计约束：server 侧不假设变体种类。变体 → 环境类型的分发完全复用
//! banqi-core 的 `for_variant_env!` 宏（core 内单一真源），所有操作实现为
//! 仅依赖 `GameEnv` / `SnapshotEnv` trait 与 `GameConfig` 的泛型函数。
//! banqi-core 新增变体时，本 crate 零改动。

use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::error::EnvError;
use banqi_core::core::env::{GameEnv, PositionSnapshot, SnapshotEnv, Variant};

/// server 侧薄包装宏：由 `(变体, 快照)` 重建泛型环境并执行 `$body`。
///
/// 展开为 `for_variant_env!` 的 match 分发 + `SnapshotEnv::from_snapshot` 精确重建。
/// `$body` 在各分支中返回相同类型 `R`，整体返回 `Result<R, EnvError>`
/// （快照载荷非法 / 变体不符时为 Err）。
///
/// ```ignore
/// let legal: Vec<usize> = with_env!(variant, &snap, env => env_step_legal(&env))?;
/// ```
#[macro_export]
macro_rules! with_env {
    ($variant:expr, $snap:expr, $e:ident => $body:expr) => {
        banqi_core::for_variant_env!($variant, E => {
            <E as banqi_core::core::env::SnapshotEnv>::from_snapshot($snap).map(|$e| {
                // 统一以可变绑定提供（部分调用点需要 step），未变异的调用点不告警
                #[allow(unused_mut)]
                let mut $e = $e;
                $body
            })
        })
    };
}

// ============================================================================
// 基础操作（泛型，仅依赖 trait）
// ============================================================================

/// 按变体创建初始局面的完整快照。
pub fn initial_snapshot(variant: Variant) -> PositionSnapshot {
    banqi_core::for_variant_env!(variant, E => {
        <E as Default>::default().to_snapshot()
    })
}

/// 当前合法动作列表（升序）。
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

/// `GameEnv::step` 的结果（与 trait 返回值一一对应，便于上层使用）。
#[derive(Clone, Copy, Debug)]
pub struct StepOutcome {
    pub terminated: bool,
    pub truncated: bool,
    /// 全局视角：Some(1)=红/先手胜，Some(-1)=黑/后手胜，Some(0)=平局，None=未结束。
    pub winner: Option<i32>,
}

impl StepOutcome {
    /// 对局是否结束（terminated 或 truncated）。
    pub fn is_over(&self) -> bool {
        self.terminated || self.truncated
    }
}

/// 执行动作（含翻棋等机会动作；机会结果由环境内部按概率随机决定）。
pub fn step<E: GameEnv>(env: &mut E, action: usize) -> Result<StepOutcome, EnvError> {
    let (_, terminated, truncated, winner) = env.step(action)?;
    Ok(StepOutcome {
        terminated,
        truncated,
        winner,
    })
}

/// 终局判定：`(terminated, truncated, winner)`。
pub fn game_over<E: GameEnv>(env: &E) -> (bool, bool, Option<i32>) {
    env.check_game_over_conditions()
}

// ============================================================================
// 动作坐标与特征维度（纯 config 推导，与具体环境类型无关）
// ============================================================================

/// 动作对应的坐标 `[from, to]`（翻棋时 from == to）。
///
/// 经 `action_lookup_tables(&config)` 公开 API 查询（core 内按 config 缓存），
/// 不依赖任何具体环境类型。
pub fn coords_for_action(variant: Variant, action: usize) -> Option<Vec<usize>> {
    action_lookup_tables(&variant.config())
        .action_to_coords
        .get(action)
        .cloned()
}
