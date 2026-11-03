//! JSON-RPC 2.0 API (`/api/rpc2`).
//!
//! The transport layer mirrors `api/jsonRpc/JsonRpc.go`:
//!
//! * `GET /api/rpc2` upgrades to a WebSocket;
//! * `POST /api/rpc2` accepts a single request or a batch;
//! * both transports share [`registry::dispatch`], namespace authorization and
//!   the error codes from `utils/rpc/errors.go`.

mod admin;
mod common;
mod context;
mod protocol;
mod registry;
mod route;

pub use context::{Permission, RpcContext};
pub use protocol::{
    ABORTED, ALREADY_EXISTS, CANCELLED, DATA_LOSS, DEADLINE_EXCEEDED, INTERNAL_ERROR,
    INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, NOT_FOUND, OUT_OF_RANGE, PARSE_ERROR,
    PERMISSION_DENIED, RPC_VERSION, RpcError, RpcRequest, RpcResponse, UNAUTHENTICATED,
    UNAVAILABLE, UNIMPLEMENTED, parse_requests,
};
pub use registry::{Method, MethodMeta, ParamMeta, dispatch, find, methods, namespace};
pub use route::{rpc_router, rpc_router_with_state};
