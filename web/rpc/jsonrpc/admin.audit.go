package jsonrpc

import (
	"context"
	"strconv"
	"time"

	"github.com/komari-monitor/komari/database/auditlog"
	"github.com/komari-monitor/komari/pkg/rpc"
)

// admin.audit.go
// 终端 / 远程执行的日志溯源查询 RPC2 方法（admin 命名空间）。

func init() {
	reg("getAuditTraces", adminGetAuditTraces, "Query terminal/exec audit traces (paged & filtered)")
}

// parseAuditFilter 从请求参数构造审计溯源过滤条件，并返回分页信息。
func parseAuditFilter(req *rpc.JsonRpcRequest) (auditlog.AuditTraceFilter, int, int, *rpc.JsonRpcError) {
	var params struct {
		Category   string `json:"category"`
		Action     string `json:"action"`
		ClientUUID string `json:"client_uuid"`
		ActorUUID  string `json:"actor_uuid"`
		SessionID  string `json:"session_id"`
		Keyword    string `json:"keyword"`
		StartTime  string `json:"start_time"`
		EndTime    string `json:"end_time"`
		Limit      string `json:"limit"`
		Page       string `json:"page"`
	}
	req.BindParams(&params)

	filter := auditlog.AuditTraceFilter{
		Category:   params.Category,
		Action:     params.Action,
		ClientUUID: params.ClientUUID,
		ActorUUID:  params.ActorUUID,
		SessionID:  params.SessionID,
		Keyword:    params.Keyword,
	}
	if params.StartTime != "" {
		if t, err := parseFlexibleTime(params.StartTime); err == nil {
			filter.StartTime = &t
		} else {
			return filter, 0, 0, rpc.MakeError(rpc.InvalidParams, "Invalid start_time: "+params.StartTime, nil)
		}
	}
	if params.EndTime != "" {
		if t, err := parseFlexibleTime(params.EndTime); err == nil {
			filter.EndTime = &t
		} else {
			return filter, 0, 0, rpc.MakeError(rpc.InvalidParams, "Invalid end_time: "+params.EndTime, nil)
		}
	}

	limit := 100
	if params.Limit != "" {
		if v, err := strconv.Atoi(params.Limit); err == nil && v > 0 {
			limit = v
		} else {
			return filter, 0, 0, rpc.MakeError(rpc.InvalidParams, "Invalid limit: "+params.Limit, nil)
		}
	}
	page := 1
	if params.Page != "" {
		if v, err := strconv.Atoi(params.Page); err == nil && v > 0 {
			page = v
		} else {
			return filter, 0, 0, rpc.MakeError(rpc.InvalidParams, "Invalid page: "+params.Page, nil)
		}
	}
	return filter, limit, (page - 1) * limit, nil
}

// parseFlexibleTime 支持 RFC3339 与 "2006-01-02 15:04:05" 两种常见输入。
func parseFlexibleTime(s string) (time.Time, error) {
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return t, nil
	}
	return time.ParseInLocation("2006-01-02 15:04:05", s, time.Local)
}

func adminGetAuditTraces(_ context.Context, req *rpc.JsonRpcRequest) (any, *rpc.JsonRpcError) {
	filter, limit, offset, rpcErr := parseAuditFilter(req)
	if rpcErr != nil {
		return nil, rpcErr
	}
	traces, total, err := auditlog.QueryAuditTraces(filter, limit, offset)
	if err != nil {
		return nil, rpc.MakeError(rpc.InternalError, "Failed to query audit traces: "+err.Error(), nil)
	}
	return map[string]any{"traces": traces, "total": total}, nil
}
