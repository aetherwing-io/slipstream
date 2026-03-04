use serde::{Deserialize, Serialize};

/// JSON-RPC request envelope
#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: Option<u64>,
    pub method: String,
    pub params: serde_json::Value,
}

/// JSON-RPC response envelope
#[derive(Debug, Serialize)]
pub struct Response {
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Option<u64>, result: impl Serialize) -> Self {
        match serde_json::to_value(result) {
            Ok(v) => Response {
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => Self::err(
                id,
                RpcError {
                    code: ERR_INTERNAL,
                    message: format!("serialization error: {e}"),
                    data: None,
                },
            ),
        }
    }

    pub fn err(id: Option<u64>, error: RpcError) -> Self {
        Response {
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// Standard JSON-RPC error codes
pub const ERR_PARSE: i32 = -32700;
#[allow(dead_code)]
pub const ERR_INVALID_REQUEST: i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_INTERNAL: i32 = -32603;

// Application error codes
pub const ERR_CONFLICT: i32 = 409;
pub const ERR_SESSION_NOT_FOUND: i32 = 404;
#[allow(dead_code)]
pub const ERR_FILE_TOO_LARGE: i32 = 413;
pub const ERR_RESOURCE_LIMIT: i32 = -32003;
