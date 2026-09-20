use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// A publication contract was violated. Exit 1.
    #[error("{0}")]
    Contract(String),
    /// Input or repository state is invalid or unavailable. Exit 2.
    #[error("{0}")]
    Invalid(String),
    /// The destination PR carries the sync-hold label. Exit 3.
    #[error("sync-hold is set on the destination pull request")]
    Held,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Contract(_) => 1,
            Error::Invalid(_) | Error::Io(_) => 2,
            Error::Held => 3,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn contract(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Contract(message.into()))
    }
}

pub fn invalid(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}
