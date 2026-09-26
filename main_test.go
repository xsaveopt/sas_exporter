package main

import (
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"
)

var baseURL string

func TestMain(m *testing.M) {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	addr := l.Addr().String()
	_ = l.Close()
	hwmon, err := os.MkdirTemp("", "hwmon")
	if err != nil {
		panic(err)
	}
	os.Args = []string{"sas_exporter", "-web.listen-address=" + addr, "-web.telemetry-path=/probe", "-hwmon.path=" + hwmon}
	go main()
	baseURL = "http://" + addr
	deadline := time.Now().Add(10 * time.Second)
	for {
		c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond)
		if err == nil {
			_ = c.Close()
			break
		}
		if time.Now().After(deadline) {
			panic("sas_exporter did not start listening")
		}
		time.Sleep(20 * time.Millisecond)
	}
	code := m.Run()
	_ = os.RemoveAll(hwmon)
	os.Exit(code)
}

type reply struct {
	status int
	header http.Header
	body   string
}

func get(t *testing.T, path string) reply {
	t.Helper()
	resp, err := http.Get(baseURL + path)
	if err != nil {
		t.Fatalf("GET %s: %v", path, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}
	return reply{status: resp.StatusCode, header: resp.Header, body: string(body)}
}

func TestHealthBeforeAnyScrape(t *testing.T) {
	r := get(t, "/health")
	if r.status != http.StatusOK {
		t.Errorf("/health status = %d, want %d", r.status, http.StatusOK)
	}
	if r.body != "up" {
		t.Errorf("/health body = %q, want %q", r.body, "up")
	}
	if ct := r.header.Get("Content-Type"); ct != "text/plain; charset=utf-8" {
		t.Errorf("/health Content-Type = %q, want text/plain; charset=utf-8", ct)
	}
}

func TestIndexLinksTheTelemetryPath(t *testing.T) {
	for _, path := range []string{"/", "/anything"} {
		r := get(t, path)
		if r.status != http.StatusOK {
			t.Errorf("%s status = %d, want %d", path, r.status, http.StatusOK)
		}
		for _, want := range []string{"<h1>SAS HBA Exporter</h1>", `<a href="/probe">Metrics</a>`, "Version: " + version} {
			if !strings.Contains(r.body, want) {
				t.Errorf("%s body is missing %q:\n%s", path, want, r.body)
			}
		}
	}
}

func TestTelemetryPathServesMetrics(t *testing.T) {
	r := get(t, "/probe")
	if r.status != http.StatusOK {
		t.Fatalf("/probe status = %d, want %d", r.status, http.StatusOK)
	}
	for _, want := range []string{"go_goroutines", "sas_exporter_tool_up{tool=\"mpt\"}"} {
		if !strings.Contains(r.body, want) {
			t.Errorf("/probe is missing %s", want)
		}
	}
}
