//! RPC method registry and dispatch.

use std::sync::LazyLock;

use serde::Serialize;
use serde_json::Value;

use super::admin;
use super::common;
use super::context::RpcContext;
use super::protocol::{INVALID_REQUEST, METHOD_NOT_FOUND, RpcError, RpcRequest, RpcResponse};

/// One parameter in a method's help metadata.
#[derive(Clone, Debug, Serialize)]
pub struct ParamMeta {
    pub name: &'static str,
    #[serde(rename = "type", skip_serializing_if = "str::is_empty")]
    pub param_type: &'static str,
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub description: &'static str,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Help metadata matching `utils/rpc/meta.go`.
#[derive(Clone, Debug, Serialize)]
pub struct MethodMeta {
    pub name: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub summary: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamMeta>,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub returns: &'static str,
}

/// A registered method.
pub struct Method {
    pub name: &'static str,
    pub meta: MethodMeta,
    pub handler: fn(&RpcContext, &RpcRequest) -> Result<Value, RpcError>,
}

static METHODS: LazyLock<Vec<Method>> = LazyLock::new(build_methods);

fn build_methods() -> Vec<Method> {
    vec![
        Method {
            name: "common:getNodes",
            meta: MethodMeta {
                name: "common:getNodes",
                summary: "Get all nodes",
                description: "",
                params: vec![ParamMeta {
                    name: "uuid",
                    param_type: "string",
                    required: false,
                    description: "Specify the UUID of the node",
                }],
                returns: "Client | { [uuid]: Client }",
            },
            handler: common::get_nodes,
        },
        Method {
            name: "common:getNodesLatestStatus",
            meta: MethodMeta {
                name: "common:getNodesLatestStatus",
                summary: "Get latest status reports (single node or map).",
                description: "",
                params: vec![
                    ParamMeta {
                        name: "uuid",
                        param_type: "string",
                        required: false,
                        description: "Specify the UUID of the node",
                    },
                    ParamMeta {
                        name: "uuids",
                        param_type: "string[]",
                        required: false,
                        description: "Specify multiple UUIDs",
                    },
                ],
                returns: "Record | { [uuid]: Record }",
            },
            handler: common::get_nodes_latest_status,
        },
        Method {
            name: "common:getMe",
            meta: MethodMeta {
                name: "common:getMe",
                summary: "Get the current identity",
                description: "",
                params: Vec::new(),
                returns: "object",
            },
            handler: common::get_me,
        },
        Method {
            name: "common:getPublicInfo",
            meta: MethodMeta {
                name: "common:getPublicInfo",
                summary: "Get public site information",
                description: "",
                params: Vec::new(),
                returns: "object",
            },
            handler: common::get_public_info,
        },
        Method {
            name: "common:getVersion",
            meta: MethodMeta {
                name: "common:getVersion",
                summary: "Get the backend version",
                description: "",
                params: Vec::new(),
                returns: "{ version, hash }",
            },
            handler: common::get_version,
        },
        Method {
            name: "common:getNodeRecentStatus",
            meta: MethodMeta {
                name: "common:getNodeRecentStatus",
                summary: "Get recent status records for one node",
                description: "",
                params: vec![ParamMeta {
                    name: "uuid",
                    param_type: "string",
                    required: true,
                    description: "Client UUID",
                }],
                returns: "{ count, records }",
            },
            handler: common::get_node_recent_status,
        },
        Method {
            name: "common:getRecords",
            meta: MethodMeta {
                name: "common:getRecords",
                summary: "Get load records grouped by client",
                description: "",
                params: vec![
                    ParamMeta {
                        name: "type",
                        param_type: "string",
                        required: false,
                        description: "Only 'load' is supported",
                    },
                    ParamMeta {
                        name: "uuid",
                        param_type: "string",
                        required: false,
                        description: "Client UUID (empty = all)",
                    },
                    ParamMeta {
                        name: "hours",
                        param_type: "int",
                        required: false,
                        description: "Time window in hours (default 1)",
                    },
                    ParamMeta {
                        name: "start",
                        param_type: "string",
                        required: false,
                        description: "RFC3339 start time",
                    },
                    ParamMeta {
                        name: "end",
                        param_type: "string",
                        required: false,
                        description: "RFC3339 end time",
                    },
                    ParamMeta {
                        name: "load_type",
                        param_type: "string",
                        required: false,
                        description: "cpu|gpu|ram|swap|load|temp|disk|network|process|connections|all",
                    },
                    ParamMeta {
                        name: "maxCount",
                        param_type: "int",
                        required: false,
                        description: "Maximum number of points (default 4000, -1 = unlimited)",
                    },
                ],
                returns: "{ count, records, from, to }",
            },
            handler: common::get_records,
        },
        Method {
            name: "admin:getClients",
            meta: MethodMeta {
                name: "admin:getClients",
                summary: "List all clients (admin extension)",
                description: "",
                params: Vec::new(),
                returns: "Client[]",
            },
            handler: admin::get_clients,
        },
        Method {
            name: "admin:getClient",
            meta: MethodMeta {
                name: "admin:getClient",
                summary: "Get one client (admin extension)",
                description: "",
                params: vec![ParamMeta {
                    name: "uuid",
                    param_type: "string",
                    required: true,
                    description: "Client UUID",
                }],
                returns: "Client",
            },
            handler: admin::get_client,
        },
        Method {
            name: "admin:getSettings",
            meta: MethodMeta {
                name: "admin:getSettings",
                summary: "Get all configuration values (admin extension)",
                description: "",
                params: Vec::new(),
                returns: "object",
            },
            handler: admin::get_settings,
        },
        Method {
            name: "admin:saveSettings",
            meta: MethodMeta {
                name: "admin:saveSettings",
                summary: "Update configuration values (admin extension)",
                description: "",
                params: Vec::new(),
                returns: "null",
            },
            handler: admin::save_settings,
        },
        Method {
            name: "admin:addClient",
            meta: MethodMeta {
                name: "admin:addClient",
                summary: "Create a client (admin extension)",
                description: "",
                params: vec![ParamMeta {
                    name: "name",
                    param_type: "string",
                    required: false,
                    description: "Client name",
                }],
                returns: "{ uuid, token }",
            },
            handler: admin::add_client,
        },
        Method {
            name: "admin:editClient",
            meta: MethodMeta {
                name: "admin:editClient",
                summary: "Edit a client (admin extension)",
                description: "",
                params: vec![ParamMeta {
                    name: "uuid",
                    param_type: "string",
                    required: true,
                    description: "Client UUID",
                }],
                returns: "null",
            },
            handler: admin::edit_client,
        },
        Method {
            name: "admin:removeClient",
            meta: MethodMeta {
                name: "admin:removeClient",
                summary: "Delete a client (admin extension)",
                description: "",
                params: vec![ParamMeta {
                    name: "uuid",
                    param_type: "string",
                    required: true,
                    description: "Client UUID",
                }],
                returns: "null",
            },
            handler: admin::remove_client,
        },
        Method {
            name: "rpc.methods",
            meta: MethodMeta {
                name: "rpc.methods",
                summary: "List methods",
                description: "Return the list of currently callable methods. By default, internal methods are not included. Pass internal=true to include them.",
                params: vec![ParamMeta {
                    name: "internal",
                    param_type: "bool",
                    required: false,
                    description: "Whether to include internal rpc.* methods",
                }],
                returns: "[]string",
            },
            handler: rpc_methods,
        },
        Method {
            name: "rpc.version",
            meta: MethodMeta {
                name: "rpc.version",
                summary: "Return the RPC version",
                description: "",
                params: Vec::new(),
                returns: "string",
            },
            handler: rpc_version,
        },
        Method {
            name: "rpc.ping",
            meta: MethodMeta {
                name: "rpc.ping",
                summary: "Health check, returns pong",
                description: "",
                params: Vec::new(),
                returns: "string",
            },
            handler: rpc_ping,
        },
        Method {
            name: "rpc.help",
            meta: MethodMeta {
                name: "rpc.help",
                summary: "Get method help",
                description: "Returns detailed metadata for the specified method if given.",
                params: vec![ParamMeta {
                    name: "method",
                    param_type: "string",
                    required: false,
                    description: "Target method name",
                }],
                returns: "MethodMeta",
            },
            handler: rpc_help,
        },
    ]
}

/// Every registered method, including `rpc.*` internals.
pub fn methods() -> &'static [Method] {
    &METHODS
}

pub fn find(name: &str) -> Option<&'static Method> {
    methods().iter().find(|method| method.name == name)
}

/// Namespace of a wire method. Methods without a colon use `common`, matching
/// Go's `strings.Split` fallback.
pub fn namespace(method: &str) -> &str {
    match method.split_once(':') {
        Some((namespace, _)) => namespace,
        None => "common",
    }
}

/// Validate, authorize and execute one request.
pub fn dispatch(context: &RpcContext, request: &RpcRequest) -> RpcResponse {
    if let Some(error) = request.validate() {
        return error.response(request.id.clone());
    }

    let namespace = namespace(&request.method);
    if !context.permission.allows_namespace(namespace) {
        // Go uses the literal HTTP-like code 401 for namespace denials on both
        // transports (not `PermissionDenied`).
        return RpcResponse::error(request.id.clone(), RpcError::new(401, "Unauthorized", None));
    }

    let Some(method) = find(&request.method) else {
        return RpcResponse::error(
            request.id.clone(),
            RpcError::new(
                METHOD_NOT_FOUND,
                "method not found",
                Some(Value::String(request.method.clone())),
            ),
        );
    };

    match (method.handler)(context, request) {
        Ok(result) => RpcResponse::success(request.id.clone(), result),
        Err(error) => error.response(request.id.clone()),
    }
}

fn rpc_methods(_context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    #[derive(Default, serde::Deserialize)]
    struct Params {
        #[serde(default)]
        internal: bool,
    }
    let params: Params = request.bind_params();
    let mut names = methods()
        .iter()
        .filter(|method| params.internal || !method.name.starts_with("rpc."))
        .map(|method| method.name)
        .collect::<Vec<_>>();
    names.sort_unstable();
    Ok(Value::Array(
        names
            .into_iter()
            .map(|name| Value::String(name.to_owned()))
            .collect(),
    ))
}

fn rpc_version(_context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    Ok(Value::String(super::protocol::RPC_VERSION.to_owned()))
}

fn rpc_ping(_context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    Ok(Value::String("pong".to_owned()))
}

fn rpc_help(_context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    #[derive(Default, serde::Deserialize)]
    struct Params {
        #[serde(default)]
        method: String,
    }
    let params: Params = request.bind_params();
    if !params.method.is_empty() {
        let Some(method) = find(&params.method) else {
            return Err(RpcError::new(INVALID_REQUEST, "method not found", None));
        };
        return serde_json::to_value(&method.meta)
            .map_err(|error| RpcError::new(INVALID_REQUEST, error.to_string(), None));
    }

    let mut metas = methods()
        .iter()
        .map(|method| {
            serde_json::json!({
                "name": method.meta.name,
                "summary": method.meta.summary,
            })
        })
        .collect::<Vec<_>>();
    metas.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
    });
    Ok(Value::Array(metas))
}
