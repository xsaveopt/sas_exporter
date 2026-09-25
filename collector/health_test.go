package collector

import (
	"testing"
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
