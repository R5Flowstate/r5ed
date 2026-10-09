use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Everything a handler can fail with, and the status each one means.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("the request carried no valid session token")]
    Unauthorized,
    #[error("the request did not come from this host")]
    Forbidden,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            // The body carries an internal message, so it must not travel.
            Self::Unavailable(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        if let Self::Internal(m) | Self::Unavailable(m) = &self {
            eprintln!("r5ed-host: {m}");
        }
        let msg = match &self {
            Self::Internal(_) | Self::Unavailable(_) => "internal error".to_string(),
            other => other.to_string(),
        };
        if let ApiError::BadRequest(m) | ApiError::NotFound(m) | ApiError::Conflict(m) = &self {
            super::secure::scrub(m);
        }
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<rpak::Error> for ApiError {
    fn from(e: rpak::Error) -> Self {
        match e {
            rpak::Error::NotIndexed { .. } => Self::NotFound(e.to_string()),
            rpak::Error::Io(_, _) | rpak::Error::RsxExit { .. } | rpak::Error::RsxSpawn(_, _) => {
                Self::Unavailable(e.to_string())
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<rbsp::Error> for ApiError {
    fn from(e: rbsp::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;
