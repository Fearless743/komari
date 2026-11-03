//! Admin-namespace RPC methods.
//!
//! The Go backend only registers `common:*` methods; these `admin:*` methods
//! are an extension requested by the Rust port's frontend integration. Their
//! result shapes reuse the admin REST representations.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::context::RpcContext;
use super::protocol::{INTERNAL_ERROR, INVALID_PARAMS, NOT_FOUND, RpcError, RpcRequest};

#[derive(Default, serde::Deserialize)]
struct UuidParams {
    #[serde(default)]
    uuid: String,
}

#[derive(Default, serde::Deserialize)]
struct NameParams {
    #[serde(default)]
    name: String,
}

pub(crate) fn get_clients(context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    let clients = context
        .db
        .clients()
        .list()
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    serde_json::to_value(clients)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))
}

pub(crate) fn get_client(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let params: UuidParams = request.bind_params();
    if params.uuid.is_empty() {
        return Err(RpcError::new(INVALID_PARAMS, "UUID is required", None));
    }
    match context.db.clients().get(&params.uuid) {
        Ok(Some(client)) => serde_json::to_value(client)
            .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None)),
        Ok(None) => Err(RpcError::new(
            NOT_FOUND,
            "Client not found",
            Some(Value::String(params.uuid)),
        )),
        Err(error) => Err(RpcError::new(INTERNAL_ERROR, error.to_string(), None)),
    }
}

pub(crate) fn get_settings(context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    let values = context
        .db
        .config()
        .get_all()
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    Ok(Value::Object(values.into_iter().collect()))
}

pub(crate) fn save_settings(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let Some(Value::Object(object)) = request.params.clone() else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Invalid params: expected an object",
            None,
        ));
    };
    let values: HashMap<String, Value> = object.into_iter().collect();
    context
        .db
        .config()
        .set_many(&values)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    Ok(Value::Null)
}

pub(crate) fn add_client(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let params: NameParams = request.bind_params();
    let client = context
        .db
        .clients()
        .create_named(params.name)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    Ok(json!({
        "uuid": client.uuid,
        "token": client.token,
    }))
}

pub(crate) fn edit_client(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let Some(Value::Object(mut object)) = request.params.clone() else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Invalid params: expected an object",
            None,
        ));
    };
    let uuid = object
        .remove("uuid")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "UUID is required", None))?;
    let patch = crate::admin::client_patch(&Value::Object(object))
        .map_err(|message| RpcError::new(INVALID_PARAMS, message, None))?;
    context
        .db
        .clients()
        .patch(&uuid, &patch)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    Ok(Value::Null)
}

pub(crate) fn remove_client(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let params: UuidParams = request.bind_params();
    if params.uuid.is_empty() {
        return Err(RpcError::new(INVALID_PARAMS, "UUID is required", None));
    }
    context
        .db
        .clients()
        .delete(&params.uuid)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
    Ok(Value::Null)
}
