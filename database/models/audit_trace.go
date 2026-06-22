package models

// AuditTrace 是面向「终端系统」与「远程执行」的结构化日志溯源记录。
//
// 相比通用的 Log（自由文本），AuditTrace 将操作者、目标客户端、命令、退出码、
// 时长等关键信息拆分为独立可检索字段，便于按客户端 / 操作者 / 时间范围过滤与导出。
type AuditTrace struct {
	ID uint `json:"id" gorm:"primaryKey;autoIncrement"`
	// Category: "terminal"（终端会话）或 "exec"（远程命令执行）
	Category string `json:"category" gorm:"type:varchar(20);index;not null"`
	// Action: terminal -> "open"/"close"/"timeout"; exec -> "dispatch"/"result"
	Action string `json:"action" gorm:"type:varchar(30);index;not null"`
	// SessionID: 终端会话 id 或远程执行的 task id，用于把同一会话/任务的多条记录串起来
	SessionID string `json:"session_id" gorm:"type:varchar(64);index"`
	// ActorUUID / ActorIP: 发起操作的用户及其来源 IP
	ActorUUID string `json:"actor_uuid" gorm:"type:varchar(36);index"`
	ActorIP   string `json:"actor_ip" gorm:"type:varchar(45)"`
	// ClientUUID: 被操作的目标客户端
	ClientUUID string `json:"client_uuid" gorm:"type:varchar(36);index"`
	// Command: 远程执行的命令内容（终端会话此字段为空）
	Command string `json:"command" gorm:"type:text"`
	// Detail: 输出 / 备注 / 额外说明（如命令执行结果、离线提示等）
	Detail string `json:"detail" gorm:"type:longtext"`
	// ExitCode: 远程执行命令的退出码（nil 表示尚未返回）
	ExitCode *int `json:"exit_code" gorm:"type:int"`
	// DurationMs: 终端会话时长（毫秒），nil 表示不适用
	DurationMs *int64 `json:"duration_ms" gorm:"type:bigint"`
	// Time: 记录产生时间
	Time LocalTime `json:"time" gorm:"autoCreateTime;index;not null"`
}

// 审计溯源使用的常量，避免在各处出现魔法字符串。
const (
	AuditCategoryTerminal = "terminal"
	AuditCategoryExec     = "exec"

	AuditActionOpen     = "open"     // 终端会话建立
	AuditActionClose    = "close"    // 终端会话正常关闭
	AuditActionTimeout  = "timeout"  // 终端会话等待被控端超时
	AuditActionDispatch = "dispatch" // 远程执行命令下发
	AuditActionResult   = "result"   // 远程执行命令结果回传
)
