use std::path::PathBuf;

/// Why an agent session stopped or a client request failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not start the agent `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the agent process has no {0} pipe")]
    MissingPipe(&'static str),
    #[error("{0} is not valid UTF-8")]
    NonUtf8Path(PathBuf),
    #[error("no terminal has the id `{0}`")]
    UnknownTerminal(String),
    #[error("Could not confirm session shutdown: {0}")]
    Shutdown(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the agent did not answer within {0:?}")]
    TimedOut(std::time::Duration),
    #[error("ACP: {0}")]
    Protocol(#[from] agent_client_protocol::Error),
}

impl From<ClientError> for agent_client_protocol::Error {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Protocol(error) => error,
            ClientError::NonUtf8Path(_) | ClientError::UnknownTerminal(_) => {
                agent_client_protocol::Error::invalid_params().data(error.to_string())
            }
            other => agent_client_protocol::Error::internal_error().data(other.to_string()),
        }
    }
}
