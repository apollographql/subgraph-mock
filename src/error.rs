use crate::state::SchemaError;
use miette::Diagnostic;

#[derive(Debug, apollo_errors::Error, Diagnostic)]
pub enum Error {
    #[error("failed to read config file")]
    #[diagnostic(code(config::io))]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("failed to parse YAML")]
    #[diagnostic(code(config::yaml))]
    Yaml {
        #[from]
        source: serde_yaml::Error,
    },

    #[error("config file must be a mapping")]
    #[diagnostic(code(config::not_a_mapping))]
    NotAMapping,

    #[error("subgraph override for '{subgraph}' must be a mapping")]
    #[diagnostic(code(config::override_not_a_mapping))]
    OverrideNotAMapping {
        /// Name of the subgraph whose override failed to parse as a mapping
        #[extension]
        subgraph: String,
    },

    #[error("invalid header name")]
    #[diagnostic(code(config::invalid_header_name))]
    InvalidHeaderName {
        #[from]
        source: hyper::header::InvalidHeaderName,
    },

    #[error("invalid header value")]
    #[diagnostic(code(config::invalid_header_value))]
    InvalidHeaderValue {
        #[from]
        source: hyper::header::InvalidHeaderValue,
    },

    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] apollo_configuration::ConfigError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Server(#[from] ServerError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Schema(#[from] SchemaError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Telemetry(#[from] apollo_opentelemetry::InitError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Errors from `mock_server_loop`'s bind/accept calls.
#[derive(Debug, apollo_errors::Error, Diagnostic)]
pub enum ServerError {
    #[error("failed to bind server socket")]
    #[diagnostic(code(server::bind))]
    Bind { source: std::io::Error },

    #[error("failed to accept connection")]
    #[diagnostic(code(server::accept))]
    Accept { source: std::io::Error },
}
