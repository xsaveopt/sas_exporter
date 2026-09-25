//go:build !linux

package sasctlbin

import "errors"

func Path() (string, error) {
	_ = binary
	return "", errors.New("sasctl only runs on linux")
}
