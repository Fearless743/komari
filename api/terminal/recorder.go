package terminal

import (
	"strings"
	"sync"

	"github.com/Fearless743/komari/database/auditlog"
)

// sessionRecorder 对一次终端会话做命令级的“录制”：
//   - 对浏览器侧发往被控端的键盘输入流做尽力而为的行缓冲，在回车时还原出一条命令；
//   - 将被控端随后回传的输出（终端回显流）归属到“当前正在运行的命令”，直到下一条命令开始，
//     从而让每条命令记录都带有其对应的返回结果，形成完整的运行流程。
//
// 已知限制：终端为裸 PTY 流，tab 补全 / 历史命令调用无法被完整还原；输出与命令的归属基于
// “回车到下一次回车”的时间窗口，属于尽力而为的近似。单条命令输出超过上限会被截断。
type sessionRecorder struct {
	userUUID   string
	userIP     string
	clientUUID string
	sessionID  string

	// 输入行缓冲（仅在输入 goroutine 中访问，无需加锁）
	buf []rune

	// 以下字段由输入、输出两个 goroutine 共同访问，需加锁
	mu      sync.Mutex
	curID   uint   // 当前正在运行命令的记录 ID，0 表示当前无活动命令
	curOut  []byte // 当前命令累计的输出
	flushed bool   // close 是否已执行
}

func newSessionRecorder(userUUID, userIP, clientUUID, sessionID string) *sessionRecorder {
	return &sessionRecorder{
		userUUID:   userUUID,
		userIP:     userIP,
		clientUUID: clientUUID,
		sessionID:  sessionID,
	}
}

// FeedInput 处理一段来自浏览器的键盘输入字节，在检测到回车时提交一条命令。
func (r *sessionRecorder) FeedInput(data []byte) {
	r.buf = parseInputLines(r.buf, data, r.commit)
}

// parseInputLines 对键盘输入流做行编辑还原：处理退格 / Ctrl-C / Ctrl-U、跳过 ANSI 转义序列，
// 在遇到回车时通过 onLine 回调输出累积的整行（含空行）。返回更新后的行缓冲。
// 抽成纯函数以便单元测试，不依赖数据库。
func parseInputLines(buf []rune, data []byte, onLine func(string)) []rune {
	runes := []rune(string(data))
	for i := 0; i < len(runes); i++ {
		c := runes[i]
		switch {
		case c == '\r' || c == '\n':
			onLine(string(buf))
			buf = buf[:0]
		case c == 0x7f || c == 0x08: // 退格 / Backspace
			if len(buf) > 0 {
				buf = buf[:len(buf)-1]
			}
		case c == 0x15: // Ctrl-U 清除整行
			buf = buf[:0]
		case c == 0x03: // Ctrl-C 放弃当前行
			buf = buf[:0]
		case c == 0x1b: // ESC，跳过 ANSI 转义序列（如方向键 \x1b[A）
			if i+1 < len(runes) && runes[i+1] == '[' {
				i += 2
				for i < len(runes) {
					t := runes[i]
					if (t >= 'A' && t <= 'Z') || (t >= 'a' && t <= 'z') || t == '~' {
						break
					}
					i++
				}
			}
		case c == '\t':
			// tab 补全结果无法预知，忽略
		case c < 0x20:
			// 其它控制字符忽略
		default:
			buf = append(buf, c)
		}
	}
	return buf
}

// commit 结束上一条命令（持久化其输出），并在命令非空时开启一条新命令记录。
func (r *sessionRecorder) commit(line string) {
	r.mu.Lock()
	defer r.mu.Unlock()

	// 结束上一条命令：回填其输出
	if r.curID != 0 {
		auditlog.UpdateTerminalOutput(r.curID, string(r.curOut))
	}
	r.curID = 0
	r.curOut = nil

	cmd := strings.TrimSpace(line)
	if cmd == "" {
		return
	}
	// 开启新命令记录，后续输出将归属到它
	r.curID = auditlog.RecordTerminalCommand(r.userUUID, r.userIP, r.clientUUID, r.sessionID, cmd)
}

// FeedOutput 处理一段被控端回传的输出，归属到当前正在运行的命令。
func (r *sessionRecorder) FeedOutput(data []byte) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.curID == 0 {
		return // 当前无活动命令（如首个提示符 / banner），不记录
	}
	if len(r.curOut) >= auditlog.MaxCommandOutputBytes {
		return
	}
	remaining := auditlog.MaxCommandOutputBytes - len(r.curOut)
	if len(data) > remaining {
		data = data[:remaining]
	}
	r.curOut = append(r.curOut, data...)
}

// Close 在会话结束时回填最后一条命令的输出。
func (r *sessionRecorder) Close() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.flushed {
		return
	}
	r.flushed = true
	if r.curID != 0 {
		auditlog.UpdateTerminalOutput(r.curID, string(r.curOut))
		r.curID = 0
	}
}
