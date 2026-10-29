pub mod ai;
pub mod app;
pub mod mcts;
pub mod nnue;
pub mod state;
pub use bq_util::shutdown_signal;
pub fn feature_torch() -> bool {
    cfg!(feature = "torch")
}
pub fn feature_onnx() -> bool {
    cfg!(feature = "onnx")
}
