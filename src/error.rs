//! 统一 API 错误类型与 HTTP 状态码映射。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 对局不存在
    #[error("对局不存在: {0}")]
    GameNotFound(String),
    /// 动作不在当前合法动作集合内
    #[error("无效动作: {action}")]
    IllegalAction { action: usize },
    /// 对局已结束，不能再走子/悔棋
    #[error("对局已结束")]
    GameOver,
    /// 对局未启用对手（本地双人 PvP）
    #[error("对局未启用对手（本地双人模式）")]
    AiDisabled,
    /// 该对局暂无 MCTS 搜索树（AI 未走过子且未手动搜索）
    #[error("暂无 MCTS 搜索树：等待 AI 走子或 POST /mcts/search")]
    MctsTreeMissing,
    /// 通用请求错误（参数非法 / 历史不足等）
    #[error("{0}")]
    BadRequest(String),
    /// 当前轮到人类玩家，不能强制对手走子
    #[error("当前轮到人类玩家，不能强制对手走子")]
    NotAiTurn,
    /// ONNX 模型未加载（与请求变体不匹配或加载失败）
    #[error("ONNX 模型不可用: {0}")]
    ModelUnavailable(String),
    /// 内部错误
    #[error("内部错误: {0}")]
    Internal(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::GameNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::IllegalAction { .. } => StatusCode::BAD_REQUEST,
            ApiError::GameOver | ApiError::AiDisabled | ApiError::NotAiTurn => {
                StatusCode::CONFLICT
            }
            ApiError::MctsTreeMissing => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::ModelUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// 稳定的机器可读错误码，便于客户端分支处理。
    fn code(&self) -> &'static str {
        match self {
            ApiError::GameNotFound(_) => "GAME_NOT_FOUND",
            ApiError::IllegalAction { .. } => "ILLEGAL_ACTION",
            ApiError::GameOver => "GAME_OVER",
            ApiError::AiDisabled => "AI_DISABLED",
            ApiError::MctsTreeMissing => "MCTS_TREE_MISSING",
            ApiError::NotAiTurn => "NOT_AI_TURN",
            ApiError::BadRequest(_) => "BAD_REQUEST",
            ApiError::ModelUnavailable(_) => "MODEL_UNAVAILABLE",
            ApiError::Internal(_) => "INTERNAL",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Json(json!({
            "error": {
                "code": self.code(),
                "message": self.to_string(),
            }
        }));
        tracing::warn!(%status, code = self.code(), "API 错误: {self}");
        (status, body).into_response()
    }
}

impl From<banqi_core::core::env::error::EnvError> for ApiError {
    fn from(e: banqi_core::core::env::error::EnvError) -> Self {
        match e {
            banqi_core::core::env::error::EnvError::IllegalAction { action } => {
                ApiError::IllegalAction { action }
            }
            other => ApiError::Internal(other.to_string()),
        }
    }
}

/// 会话核心错误（crate::app::session）→ API 错误：语义一一对应。
impl From<crate::app::session::SessionError> for ApiError {
    fn from(e: crate::app::session::SessionError) -> Self {
        match e {
            crate::app::session::SessionError::GameOver => ApiError::GameOver,
            crate::app::session::SessionError::IllegalAction { action } => {
                ApiError::IllegalAction { action }
            }
            crate::app::session::SessionError::BadRequest(s) => ApiError::BadRequest(s),
            crate::app::session::SessionError::Internal(s) => ApiError::Internal(s),
        }
    }
}
