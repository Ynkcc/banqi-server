use banqi_core::core::env::error::EnvError;
use std::fmt;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    GameOver,
    IllegalAction { action: usize },
    BadRequest(String),
    Internal(String),
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::GameOver => write!(f, "对局已结束"),
            SessionError::IllegalAction { action } => write!(f, "无效动作: {action}"),
            SessionError::BadRequest(s) => write!(f, "{s}"),
            SessionError::Internal(s) => write!(f, "内部错误: {s}"),
        }
    }
}
impl std::error::Error for SessionError {}
impl From<EnvError> for SessionError {
    fn from(e: EnvError) -> Self {
        match e {
            EnvError::IllegalAction { action } => SessionError::IllegalAction { action },
            EnvError::InvalidScenario { context } => SessionError::BadRequest(context.to_string()),
            other => SessionError::Internal(other.to_string()),
        }
    }
}
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("对局不存在: {0}")]
    GameNotFound(String),
    #[error("无效动作: {action}")]
    IllegalAction { action: usize },
    #[error("对局已结束")]
    GameOver,
    #[error("当前行棋方由用户控制（可将其槽位设为策略托管，或用 /dry-run 获取建议走法）")]
    HumanTurn,
    #[error("暂无 MCTS 搜索树：等待 AI 走子或 POST /mcts/search")]
    MctsTreeMissing,
    #[error("暂无 Expectimax 搜索树：等待 NNUE 对手走子或 POST /nnue/search")]
    NnueTreeMissing,
    #[error("{0}")]
    BadRequest(String),
    #[error("Agent 不存在: {0}")]
    AgentNotFound(String),
    #[error("Agent 变体（{agent_variant}）与对局变体（{game_variant}）不一致")]
    AgentVariantMismatch {
        agent_variant: String,
        game_variant: String,
    },
    #[error("ONNX 模型不可用: {0}")]
    ModelUnavailable(String),
    #[error("内部错误: {0}")]
    Internal(String),
}
impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::GameNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::IllegalAction { .. } => StatusCode::BAD_REQUEST,
            ApiError::GameOver | ApiError::HumanTurn => StatusCode::CONFLICT,
            ApiError::MctsTreeMissing => StatusCode::NOT_FOUND,
            ApiError::NnueTreeMissing => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::AgentNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::AgentVariantMismatch { .. } => StatusCode::CONFLICT,
            ApiError::ModelUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            ApiError::GameNotFound(_) => "GAME_NOT_FOUND",
            ApiError::IllegalAction { .. } => "ILLEGAL_ACTION",
            ApiError::GameOver => "GAME_OVER",
            ApiError::HumanTurn => "HUMAN_TURN",
            ApiError::MctsTreeMissing => "MCTS_TREE_MISSING",
            ApiError::NnueTreeMissing => "NNUE_TREE_MISSING",
            ApiError::BadRequest(_) => "BAD_REQUEST",
            ApiError::AgentNotFound(_) => "AGENT_NOT_FOUND",
            ApiError::AgentVariantMismatch { .. } => "AGENT_VARIANT_MISMATCH",
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
        use banqi_core::core::env::error::EnvError;
        match e {
            EnvError::IllegalAction { action } => ApiError::IllegalAction { action },
            EnvError::InvalidScenario { context } => ApiError::BadRequest(context.to_string()),
            other => ApiError::Internal(other.to_string()),
        }
    }
}
impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        match e {
            SessionError::GameOver => ApiError::GameOver,
            SessionError::IllegalAction { action } => ApiError::IllegalAction { action },
            SessionError::BadRequest(s) => ApiError::BadRequest(s),
            SessionError::Internal(s) => ApiError::Internal(s),
        }
    }
}
