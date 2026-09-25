BINARY  = bin/sas_exporter
EMBED   = internal/sasctlbin

VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "0.1.0")
LDFLAGS  = -ldflags "-s -w -X main.version=$(VERSION)"

.PHONY: build sasctl clean

build: sasctl
	go build $(LDFLAGS) -o $(BINARY) .

sasctl:
	$(MAKE) -C sasctl build OUT=../$(EMBED)

clean:
	rm -f $(BINARY) $(EMBED)/sasctl_linux_amd64 $(EMBED)/sasctl_linux_arm64
