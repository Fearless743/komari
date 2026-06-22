package terminal

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// TerminalRecordDir 是终端会话录像（asciicast v2）的存放目录。
const TerminalRecordDir = "./data/terminal_records"

// recorder 将一次终端会话的输入/输出落盘为 asciicast v2 (.cast) 文件，便于完整回放与溯源。
//
// 文件格式：首行为 JSON header，其后每行一个事件 [时间偏移(秒), "o"|"i", 数据]。
// "o" 表示被控端输出（Agent->Browser），"i" 表示用户输入（Browser->Agent）。
type recorder struct {
	mu    sync.Mutex
	f     *os.File
	start time.Time
}

// sanitizeSessionID 防止会话 id 被用于路径穿越（正常 id 为随机字母数字）。
func sanitizeSessionID(id string) string {
	id = strings.ReplaceAll(id, "/", "_")
	id = strings.ReplaceAll(id, "\\", "_")
	id = strings.ReplaceAll(id, "..", "_")
	return id
}

// RecordPath 返回指定会话录像文件的绝对/相对路径。
func RecordPath(sessionID string) string {
	return filepath.Join(TerminalRecordDir, sanitizeSessionID(sessionID)+".cast")
}

// newRecorder 创建并初始化一个会话录像文件；任何失败都返回 nil（录像失败不影响终端功能）。
func newRecorder(sessionID string) *recorder {
	if err := os.MkdirAll(TerminalRecordDir, 0o755); err != nil {
		return nil
	}
	f, err := os.Create(RecordPath(sessionID))
	if err != nil {
		return nil
	}
	r := &recorder{f: f, start: time.Now()}
	header, _ := json.Marshal(map[string]any{
		"version":   2,
		"width":     80,
		"height":    24,
		"timestamp": r.start.Unix(),
		"env":       map[string]string{"TERM": "xterm-256color"},
	})
	_, _ = f.Write(append(header, '\n'))
	return r
}

// write 追加一个录像事件；kind 为 "o"（输出）或 "i"（输入）。
func (r *recorder) write(kind string, data []byte) {
	if r == nil || len(data) == 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	ev, err := json.Marshal([]any{time.Since(r.start).Seconds(), kind, string(data)})
	if err != nil {
		return
	}
	_, _ = r.f.Write(append(ev, '\n'))
}

// close 关闭录像文件。
func (r *recorder) close() {
	if r == nil {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	_ = r.f.Close()
}

// CleanupOldRecords 删除超过指定天数的终端录像文件。
func CleanupOldRecords(days int) {
	if days <= 0 {
		return
	}
	threshold := time.Now().AddDate(0, 0, -days)
	entries, err := os.ReadDir(TerminalRecordDir)
	if err != nil {
		return
	}
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".cast") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if info.ModTime().Before(threshold) {
			_ = os.Remove(filepath.Join(TerminalRecordDir, e.Name()))
		}
	}
}
