/// Errors native to AgentChat. Host exceptions remain owned by the language bridge.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidInput(String),
    #[error("This channel does not support {0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Transport(String),
    #[error("{0}")]
    Handler(String),
    /// Nobody answered a prompt in time; never an approval.
    #[error("{0}")]
    Timeout(String),
    #[error("Channel is closed")]
    Closed,
    #[error("Invalid turn transition: {0}")]
    InvalidTransition(String),
}

pub type Result<T> = std::result::Result<T, Error>;
