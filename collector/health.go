package collector

import "sync"

var (
	toolStatusMu sync.Mutex
	toolStatus   = map[string]bool{}
)

func recordToolStatus(tool string, up bool) {
	toolStatusMu.Lock()
	toolStatus[tool] = up
	toolStatusMu.Unlock()
}

func AllControllerToolsDown() bool {
	toolStatusMu.Lock()
	defer toolStatusMu.Unlock()

	if len(toolStatus) == 0 {
		return false
	}
	for _, up := range toolStatus {
		if up {
			return false
		}
	}
	return true
}
