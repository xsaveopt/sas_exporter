package sasctlbin

import (
	"bytes"
	"os"
	"strconv"
	"strings"
	"testing"

	"golang.org/x/sys/unix"
)

func fdOf(t *testing.T, p string) int {
	t.Helper()
	const prefix = "/proc/self/fd/"
	if !strings.HasPrefix(p, prefix) {
		t.Fatalf("path %q does not start with %q", p, prefix)
	}
	fd, err := strconv.Atoi(strings.TrimPrefix(p, prefix))
	if err != nil {
		t.Fatalf("path %q does not end in a descriptor: %v", p, err)
	}
	return fd
}

func TestLoadRefusesAnEmptyImage(t *testing.T) {
	for _, image := range [][]byte{nil, {}} {
		p, err := load(image)
		if err == nil {
			t.Fatalf("load(%v) = %q, want an error", image, p)
		}
		if p != "" {
			t.Errorf("load(%v) path = %q, want empty", image, p)
		}
		if !strings.Contains(err.Error(), "not embedded") {
			t.Errorf("load(%v) error = %q, want it to say sasctl is not embedded", image, err)
		}
	}
}

func TestLoadWritesTheImageIntoASealedMemfd(t *testing.T) {
	image := bytes.Repeat([]byte("#!/bin/sh\nexit 0\n"), 5000)
	p, err := load(image)
	if err != nil {
		t.Fatalf("load() error = %v", err)
	}
	fd := fdOf(t, p)
	t.Cleanup(func() { _ = unix.Close(fd) })

	got, err := os.ReadFile(p)
	if err != nil {
		t.Fatalf("reading %s: %v", p, err)
	}
	if !bytes.Equal(got, image) {
		t.Fatalf("memfd holds %d bytes, want the %d byte image", len(got), len(image))
	}

	seals, err := unix.FcntlInt(uintptr(fd), unix.F_GET_SEALS, 0)
	if err != nil {
		t.Fatalf("reading seals: %v", err)
	}
	want := unix.F_SEAL_SEAL | unix.F_SEAL_SHRINK | unix.F_SEAL_GROW | unix.F_SEAL_WRITE
	if seals&want != want {
		t.Errorf("seals = %#x, want %#x set", seals, want)
	}

	if _, err := unix.Pwrite(fd, []byte("x"), 0); err == nil {
		t.Error("writing to the sealed memfd succeeded")
	}
	if err := unix.Ftruncate(fd, 1); err == nil {
		t.Error("shrinking the sealed memfd succeeded")
	}

	flags, err := unix.FcntlInt(uintptr(fd), unix.F_GETFD, 0)
	if err != nil {
		t.Fatalf("reading descriptor flags: %v", err)
	}
	if flags&unix.FD_CLOEXEC == 0 {
		t.Error("memfd is not close-on-exec")
	}
}

func TestLoadReturnsADistinctDescriptorEachCall(t *testing.T) {
	a, err := load([]byte("a"))
	if err != nil {
		t.Fatalf("load() error = %v", err)
	}
	t.Cleanup(func() { _ = unix.Close(fdOf(t, a)) })
	b, err := load([]byte("b"))
	if err != nil {
		t.Fatalf("load() error = %v", err)
	}
	t.Cleanup(func() { _ = unix.Close(fdOf(t, b)) })
	if a == b {
		t.Fatalf("load() returned %q twice", a)
	}
}

func TestPathIsStable(t *testing.T) {
	p1, err1 := Path()
	p2, err2 := Path()
	if p1 != p2 || (err1 == nil) != (err2 == nil) {
		t.Fatalf("Path() = (%q, %v) then (%q, %v), want the same result", p1, err1, p2, err2)
	}
	if len(binary) == 0 {
		if err1 == nil {
			t.Fatal("Path() error = nil with nothing embedded")
		}
		return
	}
	if err1 != nil {
		t.Fatalf("Path() error = %v", err1)
	}
	got, err := os.ReadFile(p1)
	if err != nil {
		t.Fatalf("reading %s: %v", p1, err)
	}
	if !bytes.Equal(got, binary) {
		t.Error("Path() does not hold the embedded sasctl")
	}
}
