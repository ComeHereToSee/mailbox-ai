use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("IMAP 错误: {0}")]
    Imap(String),
    #[error("SMTP 错误: {0}")]
    Smtp(String),
    #[error("POP3 错误: {0}")]
    Pop3(String),
    #[error("登录失败: {0}")]
    Auth(String),
    #[error("网络错误: {0}")]
    Net(String),
    #[error("OAuth 错误: {0}")]
    OAuth(String),
    #[error("未找到: {0}")]
    NotFound(String),
    #[error("参数错误: {0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
    /// An AI provider or local AI command line failed.
    #[error("{0}")]
    Ai(String),
    /// No usable AI model is configured (HTTP 400, code `ai_not_configured`).
    #[error("{0}")]
    AiNotConfigured(String),
    /// The request may not be sent to an AI model (e.g. an encrypted message).
    #[error("{0}")]
    AiRefused(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(e.to_string())
    }
}

impl From<async_imap::error::Error> for Error {
    fn from(e: async_imap::error::Error) -> Self {
        use async_imap::error::Error as E;
        match e {
            E::No(m) => Error::Imap(format!("服务器拒绝: {m}")),
            E::Bad(m) => Error::Imap(format!("服务器无法处理命令: {m}")),
            E::Io(io) => Error::Net(io.to_string()),
            E::ConnectionLost => Error::Net("连接已断开".into()),
            other => Error::Net(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Net(e.to_string())
    }
}

impl From<lettre::transport::smtp::Error> for Error {
    fn from(e: lettre::transport::smtp::Error) -> Self {
        Error::Smtp(e.to_string())
    }
}

impl Error {
    /// The connection is unusable (as opposed to the server refusing one command).
    pub fn is_connection(&self) -> bool {
        matches!(self, Error::Net(_) | Error::Io(_))
    }

    /// Authentication problems are not retried automatically.
    pub fn is_auth(&self) -> bool {
        matches!(self, Error::Auth(_) | Error::OAuth(_))
    }

    pub fn code(&self) -> &'static str {
        match self {
            Error::Db(_) => "db",
            Error::Io(_) => "io",
            Error::Imap(_) => "imap",
            Error::Smtp(_) => "smtp",
            Error::Pop3(_) => "pop3",
            Error::Auth(_) => "auth",
            Error::Net(_) => "network",
            Error::OAuth(_) => "oauth",
            Error::NotFound(_) => "not_found",
            Error::Invalid(_) => "invalid",
            Error::Other(_) => "other",
            Error::Ai(_) => "ai",
            Error::AiNotConfigured(_) => "ai_not_configured",
            Error::AiRefused(_) => "ai_refused",
        }
    }
}

#[macro_export]
macro_rules! bail {
    ($variant:ident, $($arg:tt)*) => {
        return Err($crate::error::Error::$variant(format!($($arg)*)))
    };
}
