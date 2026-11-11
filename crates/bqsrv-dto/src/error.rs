use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banqi_core::core::env::error::EnvError;
use serde::Serialize;
use serde_json::json;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    GameNotFound,
    IllegalAction,
    GameOver,
    HumanTurn,
    MctsTreeMissing,
    NnueTreeMissing,
    BadRequest,
    AgentNotFound,
    AgentVariantMismatch,
    ModelUnavailable,
    Internal,
}
impl ApiErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GameNotFound => "GAME_NOT_FOUND",
            Self::IllegalAction => "ILLEGAL_ACTION",
            Self::GameOver => "GAME_OVER",
            Self::HumanTurn => "HUMAN_TURN",
            Self::MctsTreeMissing => "MCTS_TREE_MISSING",
            Self::NnueTreeMissing => "NNUE_TREE_MISSING",
            Self::BadRequest => "BAD_REQUEST",
            Self::AgentNotFound => "AGENT_NOT_FOUND",
            Self::AgentVariantMismatch => "AGENT_VARIANT_MISMATCH",
            Self::ModelUnavailable => "MODEL_UNAVAILABLE",
            Self::Internal => "INTERNAL",
        }
    }
}
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
    #[error("暂无 Expectimax 搜索树：等待对手（规则 / NNUE）走子或 POST /nnue/search")]
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
    fn code(&self) -> ApiErrorCode {
        match self {
            ApiError::GameNotFound(_) => ApiErrorCode::GameNotFound,
            ApiError::IllegalAction { .. } => ApiErrorCode::IllegalAction,
            ApiError::GameOver => ApiErrorCode::GameOver,
            ApiError::HumanTurn => ApiErrorCode::HumanTurn,
            ApiError::MctsTreeMissing => ApiErrorCode::MctsTreeMissing,
            ApiError::NnueTreeMissing => ApiErrorCode::NnueTreeMissing,
            ApiError::BadRequest(_) => ApiErrorCode::BadRequest,
            ApiError::AgentNotFound(_) => ApiErrorCode::AgentNotFound,
            ApiError::AgentVariantMismatch { .. } => ApiErrorCode::AgentVariantMismatch,
            ApiError::ModelUnavailable(_) => ApiErrorCode::ModelUnavailable,
            ApiError::Internal(_) => ApiErrorCode::Internal,
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();
        let body = Json(json!({
            "error": {
                "code": code.as_str(),
                "message": self.to_string(),
            }
        }));
        tracing::warn!(%status, code = code.as_str(), "API 错误: {self}");
        (status, body).into_response()
    }
}
impl From<EnvError> for ApiError {
    fn from(e: EnvError) -> Self {
        match e {
            EnvError::IllegalAction { action } => ApiError::IllegalAction { action },
            EnvError::InvalidScenario { context } => ApiError::BadRequest(context.to_string()),
            other => ApiError::Internal(other.to_string()),
        }
    }
}
