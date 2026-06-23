package jsonrpc

import (
	"context"

	"github.com/komari-monitor/komari/database/accounts"
	"github.com/komari-monitor/komari/pkg/config"
	"github.com/komari-monitor/komari/pkg/rpc"
)

// Dispatch 是所有传输入口的统一分发点：私有站点检查 → 权限校验 → 执行方法。
// ctx 携带可选的取消/超时；meta 为调用者身份元数据（其中 Permission 为权限分组）。
// 始终返回完整的 JsonRpcResponse（包含错误）。
func Dispatch(ctx context.Context, meta *rpc.ContextMeta, req *rpc.JsonRpcRequest) *rpc.JsonRpcResponse {
	if ctx == nil {
		ctx = context.Background()
	}
	if meta == nil {
		meta = &rpc.ContextMeta{Permission: rpc.RoleGuest}
	}
	group := meta.Permission
	if group == "" {
		group = rpc.RoleGuest
	}

	// 私有站点：未登录访客一律拒绝。
	if group == rpc.RoleGuest {
		if privateSite, _ := config.GetAs[bool](config.PrivateSiteKey); privateSite {
			return rpc.ErrorResponse(req.ID, rpc.PermissionDenied, "Private site enabled, please login first", nil)
		}
	}

	// 命名空间权限校验。
	if !rpc.CheckPermission(group, req.Method) {
		return rpc.ErrorResponse(req.ID, rpc.PermissionDenied, "Permission denied", nil)
	}

	// 敏感方法（如远程执行命令）的步进式 2FA 校验。集中在此处执行，
	// 覆盖所有传输入口（REST 桥接 / /api/rpc2 直连 / WebSocket），避免依赖 REST 中间件而被旁路。
	if rpc.IsSensitiveMethod(req.Method) {
		if jerr := verifySensitive2FA(meta); jerr != nil {
			return jerr.ResponseWithID(req.ID)
		}
	}

	return rpc.CallWithContext(rpc.NewContextWithMeta(ctx, meta), req.ID, req.Method, req.Params)
}

// verifySensitive2FA 对敏感方法执行步进式 2FA 校验，语义与 REST 中间件 api.VerifySensitive2FA 一致：
//   - API Key（机器对机器凭据）豁免；
//   - 用户未配置 2FA 时放行；
//   - 否则要求随请求提供有效的一次性验证码（取自请求头/查询参数）。
//
// 校验不通过返回 PermissionDenied 错误。
func verifySensitive2FA(meta *rpc.ContextMeta) *rpc.JsonRpcError {
	if meta.IsAPIKey {
		return nil
	}
	if meta.UserUUID == "" {
		return rpc.MakeError(rpc.PermissionDenied, "2FA code is required", nil)
	}
	user, err := accounts.GetUserByUUID(meta.UserUUID)
	if err != nil {
		return rpc.MakeError(rpc.PermissionDenied, "Failed to verify identity", nil)
	}
	if user.TwoFactor == "" {
		return nil
	}
	if meta.TwoFactorCode == "" {
		return rpc.MakeError(rpc.PermissionDenied, "2FA code is required", nil)
	}
	valid, err := accounts.Verify2Fa(meta.UserUUID, meta.TwoFactorCode)
	if err != nil || !valid {
		return rpc.MakeError(rpc.PermissionDenied, "Invalid 2FA code", nil)
	}
	return nil
}

// OnInternalRequest 内部调用 RPC 方法（如服务端代码代发请求），仅携带权限分组。
// group: 调用者权限分组 (guest/client/admin)；method: "namespace:method"；params: 参数。
func OnInternalRequest(ctx context.Context, group string, method string, params interface{}) *rpc.JsonRpcResponse {
	meta := &rpc.ContextMeta{Permission: group}
	req := &rpc.JsonRpcRequest{Version: rpc.RPC_VERSION, Method: method, Params: params}
	return Dispatch(ctx, meta, req)
}
