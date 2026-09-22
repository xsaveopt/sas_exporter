package collector

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"log"
	"os"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
)

const namespace = "sas"

var (
	controllerInfoDesc = prometheus.NewDesc(
		prometheus.BuildFQName(namespace, "controller", "info"),
		"SAS controller information, always 1.",
		[]string{"controller", "type", "firmware_version", "bios_version", "pci_address"},
		nil,
	)
	deviceInfoDesc = prometheus.NewDesc(
		prometheus.BuildFQName(namespace, "physical_device", "info"),
		"SAS physical device information, always 1.",
		[]string{"controller", "enclosure", "slot", "state", "protocol", "drive_type", "manufacturer", "model", "serial"},
		nil,
	)
	deviceTempDesc = prometheus.NewDesc(
		prometheus.BuildFQName(namespace, "physical_device", "temperature_celsius"),
		"SAS physical device temperature in Celsius.",
		[]string{"controller", "enclosure", "slot", "model", "serial"},
		nil,
	)
	toolUpDesc = prometheus.NewDesc(
		prometheus.BuildFQName(namespace, "exporter", "tool_up"),
		"1 if the named tool ran successfully, 0 otherwise.",
		[]string{"tool"},
		nil,
	)
)

type controllerInfo struct {
	index           string
	controllerType  string
	firmwareVersion string
	biosVersion     string
	pciAddress      string
}

type physicalDevice struct {
	controllerIdx string
	enclosure     string
	slot          string
	state         string
	protocol      string
	driveType     string
	manufacturer  string
	model         string
	serial        string
	tempC         float64
	hasTemp       bool
}

type IrcuCollector struct {
	tools []ircuTool
}

type ircuTool struct {
	name string
	path string
}

func NewIrcuCollector(sas3ircuPath, sas2ircuPath string) *IrcuCollector {
	return &IrcuCollector{
		tools: []ircuTool{
			{name: "sas3ircu", path: sas3ircuPath},
			{name: "sas2ircu", path: sas2ircuPath},
		},
	}
}

func (c *IrcuCollector) Describe(ch chan<- *prometheus.Desc) {
	ch <- controllerInfoDesc
	ch <- deviceInfoDesc
	ch <- deviceTempDesc
	ch <- toolUpDesc
}

func (c *IrcuCollector) Collect(ch chan<- prometheus.Metric) {
	for _, tool := range c.tools {
		controllers, devices, err := scrape(tool.path)
		recordToolStatus(tool.name, err == nil)
		if err != nil {
			if !binaryNotFound(err) {
				log.Printf("sas_exporter: %s: %v", tool.name, err)
			}
			ch <- prometheus.MustNewConstMetric(toolUpDesc, prometheus.GaugeValue, 0, tool.name)
			continue
		}
		ch <- prometheus.MustNewConstMetric(toolUpDesc, prometheus.GaugeValue, 1, tool.name)

		for _, ctrl := range controllers {
			ch <- prometheus.MustNewConstMetric(
				controllerInfoDesc, prometheus.GaugeValue, 1,
				ctrl.index, ctrl.controllerType, ctrl.firmwareVersion,
				ctrl.biosVersion, ctrl.pciAddress,
			)
		}
		for _, dev := range devices {
			ch <- prometheus.MustNewConstMetric(
				deviceInfoDesc, prometheus.GaugeValue, 1,
				dev.controllerIdx, dev.enclosure, dev.slot,
				dev.state, dev.protocol, dev.driveType,
				dev.manufacturer, dev.model, dev.serial,
			)
			if dev.hasTemp {
				ch <- prometheus.MustNewConstMetric(
					deviceTempDesc, prometheus.GaugeValue, dev.tempC,
					dev.controllerIdx, dev.enclosure, dev.slot,
					dev.model, dev.serial,
				)
			}
		}
	}
}

func scrape(toolPath string) ([]controllerInfo, []physicalDevice, error) {
	out, err := runTool(toolPath, "LIST")
	if err != nil {
		return nil, nil, fmt.Errorf("running LIST: %w", err)
	}

	indices := parseIndices(out)

	var controllers []controllerInfo
	var devices []physicalDevice
	for _, idx := range indices {
		out, err := runTool(toolPath, idx, "DISPLAY")
		if err != nil {
			log.Printf("sas_exporter: %s %s DISPLAY: %v", toolPath, idx, err)
			continue
		}
		ctrl, devs := parseDisplay(idx, out)
		controllers = append(controllers, ctrl)
		devices = append(devices, devs...)
	}

	return controllers, devices, nil
}

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

func binaryNotFound(err error) bool {
	return errors.Is(err, exec.ErrNotFound) || errors.Is(err, os.ErrNotExist)
}

var listIndexRe = regexp.MustCompile(`^\s+(\d+)\s+\S`)

func parseIndices(output []byte) []string {
	var indices []string
	inTable := false
	scanner := bufio.NewScanner(bytes.NewReader(output))
	for scanner.Scan() {
		line := scanner.Text()
		if strings.Contains(line, "-----") {
			inTable = true
			continue
		}
		if inTable {
			if m := listIndexRe.FindStringSubmatch(line); m != nil {
				indices = append(indices, m[1])
			}
		}
	}
	return indices
}

func parseDisplay(idx string, output []byte) (controllerInfo, []physicalDevice) {
	ctrl := controllerInfo{index: idx}
	var devices []physicalDevice

	for section, lines := range splitSections(output) {
		switch {
		case strings.Contains(section, "Controller information"):
			kv := parseKV(lines)
			ctrl.controllerType = kv["Controller type"]
			ctrl.firmwareVersion = kv["Firmware version"]
			ctrl.biosVersion = kv["BIOS version"]
			if bus := kv["Bus"]; bus != "" {
				ctrl.pciAddress = fmt.Sprintf("%s:%s.%s", bus, kv["Device"], kv["Function"])
			}
		case strings.Contains(section, "Physical device information"):
			devices = parsePhysicalDevices(idx, lines)
		}
	}

	return ctrl, devices
}

type parseState int

const (
	stateContent parseState = iota
	stateExpectTitle
	stateExpectDash
)

func splitSections(output []byte) map[string][]string {
	sections := make(map[string][]string)
	var currentSection string
	var currentLines []string
	state := stateContent

	scanner := bufio.NewScanner(bytes.NewReader(output))
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		isDash := strings.HasPrefix(trimmed, "---")

		switch state {
		case stateContent:
			if isDash {
				if currentSection != "" {
					sections[currentSection] = currentLines
					currentSection = ""
					currentLines = nil
				}
				state = stateExpectTitle
			} else if currentSection != "" && trimmed != "" {
				currentLines = append(currentLines, line)
			}
		case stateExpectTitle:
			if !isDash && trimmed != "" {
				currentSection = trimmed
				state = stateExpectDash
			}
		case stateExpectDash:
			if isDash {
				state = stateContent
			}
		}
	}

	if currentSection != "" && len(currentLines) > 0 {
		sections[currentSection] = currentLines
	}

	return sections
}

var kvRe = regexp.MustCompile(`^\s*([^:]+?)\s*:\s*(.+?)\s*$`)

func parseKV(lines []string) map[string]string {
	kv := make(map[string]string)
	for _, line := range lines {
		if m := kvRe.FindStringSubmatch(line); m != nil {
			kv[m[1]] = m[2]
		}
	}
	return kv
}

var tempRe = regexp.MustCompile(`(\d+)C\s+\(`)

func parsePhysicalDevices(ctrlIdx string, lines []string) []physicalDevice {
	var devices []physicalDevice
	var cur *physicalDevice

	flush := func() {
		if cur != nil {
			devices = append(devices, *cur)
			cur = nil
		}
	}

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "Device is a") {
			flush()
			cur = &physicalDevice{controllerIdx: ctrlIdx}
			continue
		}
		if cur == nil {
			continue
		}
		m := kvRe.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		key, val := strings.TrimSpace(m[1]), strings.TrimSpace(m[2])
		switch key {
		case "Enclosure #":
			cur.enclosure = val
		case "Slot #":
			cur.slot = val
		case "State":

			if i := strings.Index(val, "("); i >= 0 {
				cur.state = strings.Trim(val[i:], "()")
			} else {
				cur.state = val
			}
		case "Protocol":
			cur.protocol = val
		case "Drive Type":
			cur.driveType = val
		case "Manufacturer":
			cur.manufacturer = val
		case "Model Number":
			cur.model = val
		case "Serial No":
			cur.serial = val
		case "Drive Temperature":
			if m2 := tempRe.FindStringSubmatch(val); m2 != nil {
				if t, err := strconv.ParseFloat(m2[1], 64); err == nil {
					cur.tempC = t
					cur.hasTemp = true
				}
			}
		}
	}
	flush()

	return devices
}
