use apollo_errors::FormatConfig;
use hyper::{StatusCode, body::Bytes};
use miette::Diagnostic;
use serde_json_bytes::serde_json::{self, Value, json};
use tracing::error;

#[derive(Debug, apollo_errors::Error, Diagnostic)]
pub enum HandlerError {
    #[error("malformed request body: {source}")]
    #[diagnostic(code(graphql::invalid_json))]
    #[http_status(400)]
    InvalidJson { source: serde_json::Error },

    #[error("invalid graphql query: {message}")]
    #[diagnostic(code(graphql::invalid_query))]
    #[http_status(400)]
    InvalidQuery {
        #[extension]
        message: String,
        #[extension]
        errors: Value,
    },

    #[error("{message}")]
    #[diagnostic(code(graphql::request_error))]
    #[http_status(400)]
    RequestError {
        #[extension]
        message: String,
        #[extension]
        locations: Value,
    },

    #[error("unable to generate response: {source}")]
    #[diagnostic(code(graphql::generation_failed))]
    #[http_status(500)]
    GenerationFailed {
        #[from]
        source: apollo_smith::ResponseError,
    },

    #[error("unable to serialize response: {source}")]
    #[diagnostic(code(graphql::serialization_failed))]
    #[http_status(500)]
    SerializationFailed { source: serde_json::Error },

    /// Mutations and subscriptions aren't implemented.
    #[error("{operation_type} operations are not supported")]
    #[diagnostic(code(graphql::not_implemented))]
    #[http_status(501)]
    NotImplemented {
        #[extension]
        operation_type: String,
    },
}

impl HandlerError {
    pub fn to_response(&self) -> (Bytes, StatusCode) {
        use apollo_errors::Error as _;

        let status = self.http_status();
        let rendered = self
            .to_graphql(FormatConfig::default())
            .unwrap_or_else(|source| {
                error!(%source, "failed to render error extensions; falling back to bare message");
                json!({ "message": self.to_string() })
            });
        let body = json!({ "data": Value::Null, "errors": [rendered] });
        let bytes = serde_json::to_vec(&body).unwrap_or_else(|source| {
            error!(%source, "failed to serialize error response");
            br#"{"data":null,"errors":[{"message":"internal error"}]}"#.to_vec()
        });

        (bytes.into(), status)
    }
}
