package sasctlbin

import (
	"errors"
	"fmt"
	"sync"

	"golang.org/x/sys/unix"
)

var (
	once    sync.Once
	path    string
	pathErr error
)

func Path() (string, error) {
	once.Do(func() {
		path, pathErr = load(binary)
	})
	return path, pathErr
}

func load(image []byte) (string, error) {
	if len(image) == 0 {
		return "", errors.New("sasctl is not embedded in this build")
	}
	base := unix.MFD_CLOEXEC | unix.MFD_ALLOW_SEALING
	fd, err := unix.MemfdCreate("sasctl", base|unix.MFD_EXEC)
	if errors.Is(err, unix.EINVAL) {
		fd, err = unix.MemfdCreate("sasctl", base)
	}
	if err != nil {
		return "", fmt.Errorf("creating memfd for sasctl: %w", err)
	}
	if err := writeAll(fd, image); err != nil {
		_ = unix.Close(fd)
		return "", fmt.Errorf("writing sasctl to memfd: %w", err)
	}
	seals := unix.F_SEAL_SEAL | unix.F_SEAL_SHRINK | unix.F_SEAL_GROW | unix.F_SEAL_WRITE
	if _, err := unix.FcntlInt(uintptr(fd), unix.F_ADD_SEALS, seals); err != nil {
		_ = unix.Close(fd)
		return "", fmt.Errorf("sealing sasctl memfd: %w", err)
	}
	return fmt.Sprintf("/proc/self/fd/%d", fd), nil
}

func writeAll(fd int, data []byte) error {
	for len(data) > 0 {
		n, err := unix.Write(fd, data)
		if err != nil {
			return err
		}
		data = data[n:]
	}
	return nil
}
