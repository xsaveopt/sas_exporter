package collector

import (
	"context"
	"os/exec"
	"testing"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

func resetToolStatus() {
	toolStatusMu.Lock()
	toolStatus = map[string]bool{}
	toolStatusMu.Unlock()
}

func TestAllControllerToolsDown(t *testing.T) {
	tests := []struct {
		name   string
		status map[string]bool
		want   bool
	}{
		{"no scrape yet", nil, false},
		{"all tools down", map[string]bool{"sas3ircu": false, "sas2ircu": false, "storcli": false}, true},
		{"one tool up", map[string]bool{"sas3ircu": false, "sas2ircu": true, "storcli": false}, false},
		{"all tools up", map[string]bool{"sas3ircu": true, "sas2ircu": true, "storcli": true}, false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			resetToolStatus()
			t.Cleanup(resetToolStatus)
			for tool, up := range tc.status {
				recordToolStatus(tool, up)
			}
			if got := AllControllerToolsDown(); got != tc.want {
				t.Errorf("AllControllerToolsDown() = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestIrcuCollectorRecordsToolStatus(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)

	stubRunner(t, func(_ context.Context, name string, _ ...string) ([]byte, error) {
		return nil, &exec.Error{Name: name, Err: exec.ErrNotFound}
	})

	c := NewIrcuCollector("sas3ircu", "sas2ircu")
	testutil.CollectAndCount(c, "sas_exporter_tool_up")

	if !AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = false after collect with missing binaries, want true")
	}
}

func TestStorCLICollectorRecordsToolStatus(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)

	stubRunner(t, func(_ context.Context, name string, _ ...string) ([]byte, error) {
		return nil, &exec.Error{Name: name, Err: exec.ErrNotFound}
	})

	c := NewStorCLICollector("storcli")
	testutil.CollectAndCount(c, "sas_exporter_tool_up")

	if !AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = false after collect with missing binary, want true")
	}

	resetToolStatus()
	recordToolStatus("storcli", true)
	if AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = true with storcli up, want false")
	}
}
