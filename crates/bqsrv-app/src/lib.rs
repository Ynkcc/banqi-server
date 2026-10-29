pub mod ai;
pub mod app;
pub mod mcts;
pub mod nnue;
pub mod state;
pub fn feature_torch() -> bool {
    cfg!(feature = "torch")
}
pub fn feature_onnx() -> bool {
    cfg!(feature = "onnx")
}
