package models

// CommandAudit 记录终端会话与远程执行（REC）中运行过的每一条命令，用于日志溯源与导出。
type CommandAudit struct {
	ID         uint      `json:"id" gorm:"primaryKey;autoIncrement"`
	Time       LocalTime `json:"time" gorm:"autoCreateTime;not null;index"`
	Source     string    `json:"source" gorm:"type:varchar(20);not null;index"` // terminal | exec
	UserUUID   string    `json:"user_uuid" gorm:"type:varchar(36);index"`       // 操作者（管理员）UUID
	UserIP     string    `json:"user_ip" gorm:"type:varchar(45)"`               // 操作者来源 IP
	ClientUUID string    `json:"client_uuid" gorm:"type:varchar(36);index"`     // 被控端 UUID
	SessionID  string    `json:"session_id" gorm:"type:varchar(64);index"`      // 终端会话 ID 或远程执行任务 ID
	Command    string    `json:"command" gorm:"type:text;not null"`             // 执行的命令
	ExitCode   *int      `json:"exit_code" gorm:"type:int"`                     // 退出码（仅远程执行有意义，nil 表示未完成/未知）
}
