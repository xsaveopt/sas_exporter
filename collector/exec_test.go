package collector

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func resetToolCache() {
	toolCacheMu.Lock()
	defer toolCacheMu.Unlock()
	toolCache = map[string]*toolResult{}
	toolLocks = map[string]*sync.Mutex{}
}

func stubRunner(t *testing.T, fn commandRunner) {
	t.Helper()
	prev := runCommand
	runCommand = fn
	resetToolCache()
	t.Cleanup(func() {
		runCommand = prev
		resetToolCache()
	})
}

func TestToolKey(t *testing.T) {
	tests := []struct {
		name  string
		aPath string
		aArgs []string
		bPath string
		bArgs []string
		equal bool
	}{
		{"same path and args", "/usr/sbin/sas3ircu", []string{"0", "DISPLAY"}, "/usr/sbin/sas3ircu", []string{"0", "DISPLAY"}, true},
		{"no args", "sas2ircu", nil, "sas2ircu", nil, true},
		{"different path", "sas2ircu", []string{"LIST"}, "sas3ircu", []string{"LIST"}, false},
		{"different args", "sas3ircu", []string{"0", "DISPLAY"}, "sas3ircu", []string{"1", "DISPLAY"}, false},
		{"arg count differs", "sas3ircu", []string{"LIST"}, "sas3ircu", nil, false},
		{"split args do not collide with joined", "storcli", []string{"/cALL", "show"}, "storcli", []string{"/cALL show"}, false},
		{"path boundary is not ambiguous", "storcli", []string{"show"}, "storclishow", nil, false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			a := toolKey(tc.aPath, tc.aArgs)
			b := toolKey(tc.bPath, tc.bArgs)
			if (a == b) != tc.equal {
				t.Fatalf("toolKey(%q, %q) = %q, toolKey(%q, %q) = %q, want equal=%v",
					tc.aPath, tc.aArgs, a, tc.bPath, tc.bArgs, b, tc.equal)
			}
		})
	}
}

func TestToolLockFor(t *testing.T) {
	resetToolCache()
	t.Cleanup(resetToolCache)

	first := toolLockFor("a")
	again := toolLockFor("a")
	other := toolLockFor("b")

	if first != again {
		t.Error("toolLockFor returned a different mutex for the same key")
	}
	if first == other {
		t.Error("toolLockFor returned the same mutex for different keys")
	}

	toolCacheMu.Lock()
	n := len(toolLocks)
	toolCacheMu.Unlock()
	if n != 2 {
		t.Errorf("toolLocks holds %d entries, want 2", n)
	}
}

func TestToolLockForConcurrent(t *testing.T) {
	resetToolCache()
	t.Cleanup(resetToolCache)

	const goroutines = 32
	var wg sync.WaitGroup
	start := make(chan struct{})
	locks := make([]*sync.Mutex, goroutines)

	for i := range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			locks[i] = toolLockFor("shared")
		}()
	}
	close(start)
	wg.Wait()

	for i, m := range locks {
		if m != locks[0] {
			t.Fatalf("goroutine %d got a different mutex", i)
		}
	}
}

func TestBinaryNotFound(t *testing.T) {
	_, statErr := os.Stat("testdata/definitely-absent")

	tests := []struct {
		name string
		err  error
		want bool
	}{
		{"nil", nil, false},
		{"exec not found", exec.ErrNotFound, true},
		{"exec error wrapper", &exec.Error{Name: "sas3ircu", Err: exec.ErrNotFound}, true},
		{"wrapped exec not found", fmt.Errorf("running LIST: %w", exec.ErrNotFound), true},
		{"os not exist", os.ErrNotExist, true},
		{"path error from stat", statErr, true},
		{"wrapped path error", fmt.Errorf("running LIST: %w", statErr), true},
		{"unrelated", errors.New("exit status 1"), false},
		{"wrapped unrelated", fmt.Errorf("running LIST: %w", errors.New("exit status 1")), false},
		{"permission denied", os.ErrPermission, false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := binaryNotFound(tc.err); got != tc.want {
				t.Errorf("binaryNotFound(%v) = %v, want %v", tc.err, got, tc.want)
			}
		})
	}
}

func TestRunToolReturnsRunnerOutput(t *testing.T) {
	var gotName string
	var gotArgs []string
	stubRunner(t, func(_ context.Context, name string, args ...string) ([]byte, error) {
		gotName = name
		gotArgs = args
		return []byte("output\n"), nil
	})

	out, err := runTool("/usr/sbin/sas3ircu", "0", "DISPLAY")
	if err != nil {
		t.Fatalf("runTool returned error: %v", err)
	}
	if string(out) != "output\n" {
		t.Errorf("runTool output = %q, want %q", out, "output\n")
	}
	if gotName != "/usr/sbin/sas3ircu" {
		t.Errorf("runner name = %q, want %q", gotName, "/usr/sbin/sas3ircu")
	}
	if len(gotArgs) != 2 || gotArgs[0] != "0" || gotArgs[1] != "DISPLAY" {
		t.Errorf("runner args = %q, want [0 DISPLAY]", gotArgs)
	}
}

func TestRunToolPassesDeadline(t *testing.T) {
	var hasDeadline bool
	stubRunner(t, func(ctx context.Context, _ string, _ ...string) ([]byte, error) {
		_, hasDeadline = ctx.Deadline()
		return nil, nil
	})

	if _, err := runTool("sas3ircu", "LIST"); err != nil {
		t.Fatalf("runTool returned error: %v", err)
	}
	if !hasDeadline {
		t.Error("runTool called the runner with a context that has no deadline")
	}
}

func TestRunToolCachesByKey(t *testing.T) {
	var calls atomic.Int64
	stubRunner(t, func(_ context.Context, _ string, args ...string) ([]byte, error) {
		calls.Add(1)
		return []byte(args[0]), nil
	})

	for range 3 {
		out, err := runTool("sas3ircu", "LIST")
		if err != nil {
			t.Fatalf("runTool returned error: %v", err)
		}
		if string(out) != "LIST" {
			t.Fatalf("runTool output = %q, want %q", out, "LIST")
		}
	}
	if got := calls.Load(); got != 1 {
		t.Errorf("runner called %d times for one key, want 1", got)
	}

	if _, err := runTool("sas3ircu", "0", "DISPLAY"); err != nil {
		t.Fatalf("runTool returned error: %v", err)
	}
	if got := calls.Load(); got != 2 {
		t.Errorf("runner called %d times for two keys, want 2", got)
	}

	if _, err := runTool("sas2ircu", "LIST"); err != nil {
		t.Fatalf("runTool returned error: %v", err)
	}
	if got := calls.Load(); got != 3 {
		t.Errorf("runner called %d times across two tools, want 3", got)
	}
}

func TestRunToolCachesFailure(t *testing.T) {
	var calls atomic.Int64
	want := errors.New("boom")
	stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
		calls.Add(1)
		return nil, want
	})

	for range 2 {
		if _, err := runTool("sas3ircu", "LIST"); !errors.Is(err, want) {
			t.Fatalf("runTool error = %v, want %v", err, want)
		}
	}
	if got := calls.Load(); got != 1 {
		t.Errorf("runner called %d times, want 1 (failures should be cached)", got)
	}
}

func TestRunToolSingleFlight(t *testing.T) {
	var calls atomic.Int64
	release := make(chan struct{})
	stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
		calls.Add(1)
		<-release
		return []byte("shared"), nil
	})

	const goroutines = 16
	var wg, started sync.WaitGroup
	results := make([][]byte, goroutines)
	for i := range goroutines {
		wg.Add(1)
		started.Add(1)
		go func() {
			defer wg.Done()
			started.Done()
			out, err := runTool("sas3ircu", "LIST")
			if err != nil {
				t.Errorf("runTool returned error: %v", err)
			}
			results[i] = out
		}()
	}
	started.Wait()
	close(release)
	wg.Wait()

	if got := calls.Load(); got != 1 {
		t.Errorf("runner called %d times under concurrency, want 1", got)
	}
	for i, out := range results {
		if string(out) != "shared" {
			t.Errorf("goroutine %d got %q, want %q", i, out, "shared")
		}
	}
}

func TestRunToolCacheTTL(t *testing.T) {
	tests := []struct {
		name string
		age  time.Duration
		want string
	}{
		{"within ttl", toolCacheTTL / 2, "cached"},
		{"expired", toolCacheTTL * 2, "fresh"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
				return []byte("fresh"), nil
			})

			key := toolKey("sas3ircu", []string{"LIST"})
			toolCacheMu.Lock()
			toolCache[key] = &toolResult{out: []byte("cached"), at: time.Now().Add(-tc.age)}
			toolCacheMu.Unlock()

			out, err := runTool("sas3ircu", "LIST")
			if err != nil {
				t.Fatalf("runTool returned error: %v", err)
			}
			if string(out) != tc.want {
				t.Errorf("runTool output = %q, want %q", out, tc.want)
			}
		})
	}
}
