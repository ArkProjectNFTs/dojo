use jsonrpsee::core::Error as RpcError;
use jsonrpsee::types::error::CallError;
use jsonrpsee::types::ErrorObject;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SolisApiError {
    #[error("Authentication failed")]
    AuthenticationFailed,

    #[error("Hooker service is unavailable")]
    HookerServiceUnavailable,

    #[error("An unexpected error occurred: {0}")]
    UnexpectedError(String),
}

impl SolisApiError {
    pub fn to_rpc_error(&self) -> RpcError {
        let error_code = match self {
            SolisApiError::AuthenticationFailed => -32000,
            SolisApiError::HookerServiceUnavailable => -32001,
            SolisApiError::UnexpectedError(_) => -32002,
        };
        let message = self.to_string();
        let error_object = ErrorObject::owned(error_code, message, None::<()>);
        RpcError::Call(CallError::Custom(error_object))
    }
}
