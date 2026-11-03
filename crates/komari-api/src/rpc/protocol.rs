//! JSON-RPC 2.0 wire types and error codes (`utils/rpc/**`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version returned in every response.
pub const RPC_VERSION: &str = "2.0";

// JSON-RPC 2.0 standard codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

// Komari codes (`utils/rpc/errors.go`).
pub const NOT_FOUND: i64 = -32044;
pub const ALREADY_EXISTS: i64 = -32045;
pub const PERMISSION_DENIED: i64 = -32041;
pub const UNAUTHENTICATED: i64 = -32040;
pub const CANCELLED: i64 = -32010;
pub const DEADLINE_EXCEEDED: i64 = -32011;
pub const ABORTED: i64 = -32021;
pub const OUT_OF_RANGE: i64 = -32022;
pub const UNIMPLEMENTED: i64 = -32050;
pub const UNAVAILABLE: i64 = -32051;
pub const DATA_LOSS: i64 = -32052;

/// One JSON-RPC request (single or a batch element).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub id: Option<Value>,
}

impl RpcRequest {
    /// Go `JsonRpcRequest.Validate`.
    pub fn validate(&self) -> Option<RpcError> {
        if self.jsonrpc != RPC_VERSION {
            return Some(RpcError::new(
                INVALID_REQUEST,
                "invalid jsonrpc version",
                None,
            ));
        }
        if self.method.trim().is_empty() {
            return Some(RpcError::new(INVALID_REQUEST, "method required", None));
        }
        None
    }

    /// Deserialize named parameters into `T`. Missing/invalid parameters leave
    /// `T` at its default value, matching the handlers' use of `BindParams`.
    pub fn bind_params<T: serde::de::DeserializeOwned + Default>(&self) -> T {
        let Some(params) = self.params.as_ref() else {
            return T::default();
        };
        match params {
            Value::Object(_) => serde_json::from_value(params.clone()).unwrap_or_default(),
            // Positional parameters are accepted when the target is a
            // sequence; struct targets keep their default value.
            Value::Array(_) => serde_json::from_value(params.clone()).unwrap_or_default(),
            _ => T::default(),
        }
    }
}

/// One JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }

    pub fn response(&self, id: Option<Value>) -> RpcResponse {
        RpcResponse {
            jsonrpc: RPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(self.clone()),
        }
    }
}

/// One JSON-RPC response.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// Go `SuccessResponse`; a JSON `null` result is omitted like `omitempty`.
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: RPC_VERSION.to_owned(),
            id,
            result: (!result.is_null()).then_some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, error: RpcError) -> Self {
        Self {
            jsonrpc: RPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Parse a single or batch request (`utils/rpc/parse.go`).
pub fn parse_requests(data: &[u8]) -> Result<Vec<RpcRequest>, RpcError> {
    let trimmed = trim_ascii(data);
    if trimmed.is_empty() {
        return Err(RpcError::new(PARSE_ERROR, "empty body", None));
    }
    match trimmed[0] {
        b'{' => {
            let request: RpcRequest = serde_json::from_slice(trimmed).map_err(|error| {
                RpcError::new(
                    PARSE_ERROR,
                    "invalid json",
                    Some(Value::String(error.to_string())),
                )
            })?;
            if let Some(error) = request.validate() {
                return Err(error);
            }
            Ok(vec![request])
        }
        b'[' => {
            let requests: Vec<RpcRequest> = serde_json::from_slice(trimmed).map_err(|error| {
                RpcError::new(
                    PARSE_ERROR,
                    "invalid json",
                    Some(Value::String(error.to_string())),
                )
            })?;
            if requests.is_empty() {
                return Err(RpcError::new(INVALID_REQUEST, "empty batch", None));
            }
            for request in &requests {
                if let Some(error) = request.validate() {
                    return Err(error);
                }
            }
            Ok(requests)
        }
        _ => Err(RpcError::new(
            PARSE_ERROR,
            "invalid json: not object/array",
            None,
        )),
    }
}

fn trim_ascii(mut data: &[u8]) -> &[u8] {
    while data.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        data = &data[1..];
    }
    while data.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        data = &data[..data.len() - 1];
    }
    data
}
