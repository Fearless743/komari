package terminal

import (
	"reflect"
	"testing"
)

func collect(inputs ...[]byte) []string {
	var got []string
	r := newCommandRecorder(func(cmd string) { got = append(got, cmd) })
	for _, in := range inputs {
		r.Feed(in)
	}
	return got
}

func TestRecorderBasicCommand(t *testing.T) {
	got := collect([]byte("ls -la\r"))
	want := []string{"ls -la"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func TestRecorderBackspace(t *testing.T) {
	// 输入 "lx" 退格删除 "x" 后输入 "s"，回车 -> "ls"
	got := collect([]byte("lx\x7fs\n"))
	want := []string{"ls"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func TestRecorderCtrlCDiscardsLine(t *testing.T) {
	got := collect([]byte("rm -rf /\x03"), []byte("whoami\r"))
	want := []string{"whoami"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func TestRecorderSkipsArrowKeys(t *testing.T) {
	// 中间夹带上方向键转义序列 \x1b[A，不应进入命令
	got := collect([]byte("ec\x1b[Aho hi\r"))
	want := []string{"echo hi"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func TestRecorderSplitAcrossFeeds(t *testing.T) {
	got := collect([]byte("cat "), []byte("/etc/hosts"), []byte("\r"))
	want := []string{"cat /etc/hosts"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func TestRecorderEmptyLinesIgnored(t *testing.T) {
	got := collect([]byte("\r\r  \r"))
	if len(got) != 0 {
		t.Fatalf("expected no commands, got %v", got)
	}
}
