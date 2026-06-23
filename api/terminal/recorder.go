package terminal

import "strings"

// commandRecorder 对浏览器侧发往被控端的终端键盘输入流进行尽力而为（best-effort）的行缓冲，
// 在用户按下回车时还原出一条完整命令并回调。它处理常见的行编辑控制符（退格、Ctrl-C、Ctrl-U）
// 并跳过 ANSI 转义序列（方向键等），从而尽可能准确地记录实际执行的命令。
//
// 注意：由于终端是裸 PTY 流，tab 补全、历史命令调用等无法被完整还原，这属于已知限制。
type commandRecorder struct {
	buf       []rune
	onCommand func(string)
}

func newCommandRecorder(onCommand func(string)) *commandRecorder {
	return &commandRecorder{onCommand: onCommand}
}

func (r *commandRecorder) flush() {
	cmd := strings.TrimSpace(string(r.buf))
	r.buf = r.buf[:0]
	if cmd != "" && r.onCommand != nil {
		r.onCommand(cmd)
	}
}

// Feed 处理一段来自浏览器的键盘输入字节。
func (r *commandRecorder) Feed(data []byte) {
	runes := []rune(string(data))
	for i := 0; i < len(runes); i++ {
		c := runes[i]
		switch {
		case c == '\r' || c == '\n':
			r.flush()
		case c == 0x7f || c == 0x08: // 退格 / Backspace
			if len(r.buf) > 0 {
				r.buf = r.buf[:len(r.buf)-1]
			}
		case c == 0x15: // Ctrl-U 清除整行
			r.buf = r.buf[:0]
		case c == 0x03: // Ctrl-C 放弃当前行
			r.buf = r.buf[:0]
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
			r.buf = append(r.buf, c)
		}
	}
}
