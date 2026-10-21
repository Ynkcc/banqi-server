//! 变体无关的环境操作层：实现已收敛到 `banqi_app::envops`（单一真源）。
//!
//! 本模块仅 re-export，保持既有引用路径（`crate::variant::*`）稳定：
//! - `with_env!` 宏由 banqi-app `#[macro_export]` 导出（`banqi_app::with_env!`），
//!   使用处显式 `use banqi_app::with_env;`；
//! - `for_variant_env!` 分发在 banqi-core / banqi-app 内完成，
//!   banqi-core 新增变体时本 crate 零改动。

pub use banqi_app::envops::{coords_for_action, StepOutcome};
