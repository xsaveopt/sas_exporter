package collector

import (
	"context"
	"os/exec"
	"strings"
	"sync"
	"time"
)

const toolCacheTTL = 30 * time.Second

type toolResult struct {
	out []byte
	err error
	at  time.Time
}

var (
	toolCacheMu sync.Mutex
	toolCache   = map[string]*toolResult{}
	toolLocks   = map[string]*sync.Mutex{}
)

type commandRunner func(ctx context.Context, name string, args ...string) ([]byte, error)

func execCommand(ctx context.Context, name string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = "/tmp"
	return cmd.Output()
}

var runCommand commandRunner = execCommand

func toolKey(toolPath string, args []string) string {
	return toolPath + "\x00" + strings.Join(args, "\x00")
}

func toolLockFor(key string) *sync.Mutex {
	toolCacheMu.Lock()
	defer toolCacheMu.Unlock()
	m, ok := toolLocks[key]
	if !ok {
		m = &sync.Mutex{}
		toolLocks[key] = m
	}
	return m
}

func runTool(toolPath string, args ...string) ([]byte, error) {
	key := toolKey(toolPath, args)

	toolCacheMu.Lock()
	if r, ok := toolCache[key]; ok && time.Since(r.at) < toolCacheTTL {
		toolCacheMu.Unlock()
		return r.out, r.err
	}
	toolCacheMu.Unlock()

	lock := toolLockFor(key)
	lock.Lock()
	defer lock.Unlock()

	toolCacheMu.Lock()
	if r, ok := toolCache[key]; ok && time.Since(r.at) < toolCacheTTL {
		toolCacheMu.Unlock()
		return r.out, r.err
	}
	toolCacheMu.Unlock()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	out, err := runCommand(ctx, toolPath, args...)

	toolCacheMu.Lock()
	toolCache[key] = &toolResult{out: out, err: err, at: time.Now()}
	toolCacheMu.Unlock()

	return out, err
}
