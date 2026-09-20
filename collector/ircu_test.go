package collector

import (
	"context"
	"errors"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func readFixture(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatalf("reading fixture: %v", err)
	}
	return data
}

func TestParseIndices(t *testing.T) {
	tests := []struct {
		name    string
		fixture string
		output  string
		want    []string
	}{
		{name: "sas3ircu single adapter", fixture: "sas3ircu_list.txt", want: []string{"0"}},
		{name: "sas2ircu two adapters", fixture: "sas2ircu_list.txt", want: []string{"0", "1"}},
		{name: "empty output", output: ""},
		{name: "header only", output: " Index    Type          ID\n   0     SAS3008\n"},
		{name: "no numbered rows", output: " -----  ------\nSAS3IRCU: Utility Completed Successfully.\n"},
		{name: "unindented row is ignored", output: " -----  ------\n0     SAS3008\n"},
		{name: "index needs a following field", output: " -----  ------\n   0   \n   1     SAS3008\n", want: []string{"1"}},
		{name: "rows before the rule are ignored", output: "   9     SAS9999\n -----\n   3     SAS3008\n", want: []string{"3"}},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			output := []byte(tc.output)
			if tc.fixture != "" {
				output = readFixture(t, tc.fixture)
			}
			got := parseIndices(output)
			if !slices.Equal(got, tc.want) {
				t.Errorf("parseIndices = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSplitSectionsFixture(t *testing.T) {
	sections := splitSections(readFixture(t, "sas3ircu_display_0.txt"))

	want := []string{"Controller information", "IR Volume information", "Physical device information", "Enclosure information"}
	for _, name := range want {
		if _, ok := sections[name]; !ok {
			t.Errorf("section %q missing, got %q", name, slices.Sorted(maps.Keys(sections)))
		}
	}
	if len(sections) != len(want) {
		t.Errorf("got %d sections %q, want %d", len(sections), slices.Sorted(maps.Keys(sections)), len(want))
	}

	for _, line := range sections["Controller information"] {
		if strings.TrimSpace(line) == "" {
			t.Error("blank line kept in section body")
		}
		if strings.HasPrefix(strings.TrimSpace(line), "---") {
			t.Errorf("rule line kept in section body: %q", line)
		}
	}
	if first := sections["Controller information"][0]; !strings.Contains(first, "Controller type") {
		t.Errorf("first controller line = %q, want the controller type", first)
	}
	if got := len(sections["Controller information"]); got != 13 {
		t.Errorf("controller section has %d lines, want 13", got)
	}
}

func TestSplitSections(t *testing.T) {
	tests := []struct {
		name   string
		output string
		want   map[string][]string
	}{
		{
			name:   "empty",
			output: "",
			want:   map[string][]string{},
		},
		{
			name:   "preamble without a rule is dropped",
			output: "LSI Corporation SAS3 IR Configuration Utility.\nVersion 17.00.00.00\n",
			want:   map[string][]string{},
		},
		{
			name:   "single section",
			output: "----\nAlpha\n----\n  a : 1\n",
			want:   map[string][]string{"Alpha": {"  a : 1"}},
		},
		{
			name:   "unterminated final section is kept",
			output: "----\nAlpha\n----\n  a : 1\n  b : 2\n",
			want:   map[string][]string{"Alpha": {"  a : 1", "  b : 2"}},
		},
		{
			name:   "empty final section is dropped",
			output: "----\nAlpha\n----\n  a : 1\n----\nSAS3IRCU: Command DISPLAY Completed Successfully.\n",
			want:   map[string][]string{"Alpha": {"  a : 1"}},
		},
		{
			name:   "two sections",
			output: "----\nAlpha\n----\n  a : 1\n----\nBeta\n----\n  b : 2\n",
			want:   map[string][]string{"Alpha": {"  a : 1"}, "Beta": {"  b : 2"}},
		},
		{
			name:   "blank lines between the rule and the title",
			output: "----\n\nAlpha\n----\n  a : 1\n",
			want:   map[string][]string{"Alpha": {"  a : 1"}},
		},
		{
			name:   "blank lines inside a body are dropped",
			output: "----\nAlpha\n----\n  a : 1\n\n  b : 2\n",
			want:   map[string][]string{"Alpha": {"  a : 1", "  b : 2"}},
		},
		{
			name:   "duplicate titles collapse to the last body",
			output: "----\nAlpha\n----\n  a : 1\n----\nAlpha\n----\n  a : 2\n",
			want:   map[string][]string{"Alpha": {"  a : 2"}},
		},
		{
			name:   "indented rules are still rules",
			output: "   ----\n   Alpha\n   ----\n  a : 1\n",
			want:   map[string][]string{"Alpha": {"  a : 1"}},
		},
		{
			name:   "two dashes are not a rule",
			output: "--\nAlpha\n--\n  a : 1\n",
			want:   map[string][]string{},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := splitSections([]byte(tc.output))
			if !reflect.DeepEqual(got, tc.want) {
				t.Errorf("splitSections = %#v, want %#v", got, tc.want)
			}
		})
	}
}

func TestParseKV(t *testing.T) {
	tests := []struct {
		name  string
		lines []string
		want  map[string]string
	}{
		{name: "nil input", want: map[string]string{}},
		{
			name:  "padding is stripped",
			lines: []string{"  Controller type                         : SAS3008"},
			want:  map[string]string{"Controller type": "SAS3008"},
		},
		{
			name:  "value keeps inner colons",
			lines: []string{"  Logical ID                              : 500605b0:0879f0c0"},
			want:  map[string]string{"Logical ID": "500605b0:0879f0c0"},
		},
		{
			name:  "key keeps inner punctuation",
			lines: []string{"  Size (in MB)/(in sectors)               : 3815447/7814037167"},
			want:  map[string]string{"Size (in MB)/(in sectors)": "3815447/7814037167"},
		},
		{
			name:  "lines without a colon are skipped",
			lines: []string{"Initiator at ID #0", "Device is a Hard disk"},
			want:  map[string]string{},
		},
		{
			name:  "empty value is skipped",
			lines: []string{"  Physical hard disks                     :"},
			want:  map[string]string{},
		},
		{
			name:  "value with trailing spaces is trimmed",
			lines: []string{"  Boot                                    : Primary   "},
			want:  map[string]string{"Boot": "Primary"},
		},
		{
			name:  "later line wins",
			lines: []string{"  Enclosure#  : 1", "  Enclosure#  : 2"},
			want:  map[string]string{"Enclosure#": "2"},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := parseKV(tc.lines)
			if !reflect.DeepEqual(got, tc.want) {
				t.Errorf("parseKV = %#v, want %#v", got, tc.want)
			}
		})
	}
}

func TestParsePhysicalDevices(t *testing.T) {
	tests := []struct {
		name  string
		lines []string
		want  []physicalDevice
	}{
		{name: "nil input"},
		{
			name:  "fields before the first device header are ignored",
			lines: []string{"Initiator at ID #0", "  Slot #  : 3"},
		},
		{
			name: "state drops the long form",
			lines: []string{
				"Device is a Hard disk",
				"  State                                   : Ready (RDY)",
			},
			want: []physicalDevice{{controllerIdx: "0", state: "RDY"}},
		},
		{
			name: "state without parentheses is kept whole",
			lines: []string{
				"Device is a Hard disk",
				"  State                                   : Optimal",
			},
			want: []physicalDevice{{controllerIdx: "0", state: "Optimal"}},
		},
		{
			name: "temperature is parsed from the celsius field",
			lines: []string{
				"Device is a Hard disk",
				"  Drive Temperature                       : 34C (93.20F)",
			},
			want: []physicalDevice{{controllerIdx: "0", tempC: 34, hasTemp: true}},
		},
		{
			name: "unparseable temperature leaves hasTemp false",
			lines: []string{
				"Device is a Hard disk",
				"  Drive Temperature                       : N/A",
			},
			want: []physicalDevice{{controllerIdx: "0"}},
		},
		{
			name: "unknown keys are ignored",
			lines: []string{
				"Device is a Hard disk",
				"  Firmware Revision                       : 0004",
				"  GUID                                    : 5000c50085e7bd3f",
			},
			want: []physicalDevice{{controllerIdx: "0"}},
		},
		{
			name: "two devices are split on the header",
			lines: []string{
				"Device is a Hard disk",
				"  Slot #                                  : 0",
				"Device is a Hard disk",
				"  Slot #                                  : 1",
			},
			want: []physicalDevice{
				{controllerIdx: "0", slot: "0"},
				{controllerIdx: "0", slot: "1"},
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := parsePhysicalDevices("0", tc.lines)
			if !reflect.DeepEqual(got, tc.want) {
				t.Errorf("parsePhysicalDevices = %#v, want %#v", got, tc.want)
			}
		})
	}
}

func TestParseDisplaySAS3(t *testing.T) {
	ctrl, devices := parseDisplay("0", readFixture(t, "sas3ircu_display_0.txt"))

	wantCtrl := controllerInfo{
		index:           "0",
		controllerType:  "SAS3008",
		firmwareVersion: "16.00.01.00",
		biosVersion:     "8.29.02.00",
		pciAddress:      "3:0.0",
	}
	if ctrl != wantCtrl {
		t.Errorf("controller = %#v, want %#v", ctrl, wantCtrl)
	}

	wantDevices := []physicalDevice{
		{
			controllerIdx: "0", enclosure: "2", slot: "0", state: "RDY",
			protocol: "SAS", driveType: "SAS_HDD", manufacturer: "SEAGATE",
			model: "ST4000NM0023", serial: "Z1Z3ABCD", tempC: 34, hasTemp: true,
		},
		{
			controllerIdx: "0", enclosure: "2", slot: "1", state: "SBY",
			protocol: "SAS", driveType: "SAS_HDD", manufacturer: "SEAGATE",
			model: "ST4000NM0023", serial: "Z1Z3EFGH", tempC: 31, hasTemp: true,
		},
		{
			controllerIdx: "0", enclosure: "2", slot: "24", state: "SBY",
			protocol: "SAS", manufacturer: "LSI", model: "SAS3x28", serial: "x36552418",
		},
	}
	if !reflect.DeepEqual(devices, wantDevices) {
		t.Errorf("devices = %#v, want %#v", devices, wantDevices)
	}
}

func TestParseDisplaySAS2(t *testing.T) {
	ctrl, devices := parseDisplay("0", readFixture(t, "sas2ircu_display_0.txt"))

	wantCtrl := controllerInfo{
		index:           "0",
		controllerType:  "SAS2008",
		firmwareVersion: "20.00.07.00",
		biosVersion:     "7.39.02.00",
		pciAddress:      "2:0.0",
	}
	if ctrl != wantCtrl {
		t.Errorf("controller = %#v, want %#v", ctrl, wantCtrl)
	}

	wantDevices := []physicalDevice{
		{
			controllerIdx: "0", enclosure: "1", slot: "4", state: "RDY",
			protocol: "SAS", driveType: "SAS_HDD", manufacturer: "HGST",
			model: "HUS726020AL4210", serial: "K4H1ABCD", tempC: 29, hasTemp: true,
		},
		{
			controllerIdx: "0", enclosure: "1", slot: "5", state: "OPT",
			protocol: "SATA", driveType: "SATA_SSD", manufacturer: "ATA",
			model: "Samsung SSD 860", serial: "S3Z2NB0K123456A",
		},
	}
	if !reflect.DeepEqual(devices, wantDevices) {
		t.Errorf("devices = %#v, want %#v", devices, wantDevices)
	}
}

func TestParseDisplayNoDevices(t *testing.T) {
	ctrl, devices := parseDisplay("1", readFixture(t, "sas2ircu_display_1.txt"))

	if ctrl.pciAddress != "129:0.0" {
		t.Errorf("pciAddress = %q, want %q", ctrl.pciAddress, "129:0.0")
	}
	if ctrl.controllerType != "SAS2308" {
		t.Errorf("controllerType = %q, want %q", ctrl.controllerType, "SAS2308")
	}
	if devices != nil {
		t.Errorf("devices = %#v, want none", devices)
	}
}

func TestParseDisplayWithoutBus(t *testing.T) {
	output := "----\nController information\n----\n  Controller type : SAS3008\n  Firmware version : 16.00.01.00\n"
	ctrl, devices := parseDisplay("0", []byte(output))

	if ctrl.pciAddress != "" {
		t.Errorf("pciAddress = %q, want empty when the bus is absent", ctrl.pciAddress)
	}
	if ctrl.controllerType != "SAS3008" {
		t.Errorf("controllerType = %q, want %q", ctrl.controllerType, "SAS3008")
	}
	if devices != nil {
		t.Errorf("devices = %#v, want none", devices)
	}
}

func TestIrcuCollectorDescribe(t *testing.T) {
	ch := make(chan *prometheus.Desc, 16)
	NewIrcuCollector("sas3ircu", "sas2ircu").Describe(ch)
	close(ch)

	var got []string
	for d := range ch {
		got = append(got, d.String())
	}
	want := []string{
		controllerInfoDesc.String(),
		deviceInfoDesc.String(),
		deviceTempDesc.String(),
		toolUpDesc.String(),
	}
	if !slices.Equal(got, want) {
		t.Errorf("Describe sent %d descriptors %v, want %v", len(got), got, want)
	}
}

func fixtureRunner(t *testing.T, responses map[string]string) commandRunner {
	t.Helper()
	return func(_ context.Context, name string, args ...string) ([]byte, error) {
		fixture, ok := responses[toolKey(name, args)]
		if !ok {
			return nil, &exec.Error{Name: name, Err: exec.ErrNotFound}
		}
		return readFixture(t, fixture), nil
	}
}

func TestIrcuCollectorCollect(t *testing.T) {
	stubRunner(t, fixtureRunner(t, map[string]string{
		toolKey("sas3ircu", []string{"LIST"}):         "sas3ircu_list.txt",
		toolKey("sas3ircu", []string{"0", "DISPLAY"}): "sas3ircu_display_0.txt",
		toolKey("sas2ircu", []string{"LIST"}):         "sas2ircu_list.txt",
		toolKey("sas2ircu", []string{"0", "DISPLAY"}): "sas2ircu_display_0.txt",
		toolKey("sas2ircu", []string{"1", "DISPLAY"}): "sas2ircu_display_1.txt",
	}))

	c := NewIrcuCollector("sas3ircu", "sas2ircu")

	if got := testutil.CollectAndCount(c, "sas_controller_info"); got != 3 {
		t.Errorf("controller info series = %d, want 3", got)
	}
	if got := testutil.CollectAndCount(c, "sas_physical_device_info"); got != 5 {
		t.Errorf("device info series = %d, want 5", got)
	}
	if got := testutil.CollectAndCount(c, "sas_physical_device_temperature_celsius"); got != 3 {
		t.Errorf("device temperature series = %d, want 3", got)
	}

	expected := `
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="sas2ircu"} 1
sas_exporter_tool_up{tool="sas3ircu"} 1
# HELP sas_physical_device_temperature_celsius SAS physical device temperature in Celsius.
# TYPE sas_physical_device_temperature_celsius gauge
sas_physical_device_temperature_celsius{controller="0",enclosure="1",model="HUS726020AL4210",serial="K4H1ABCD",slot="4"} 29
sas_physical_device_temperature_celsius{controller="0",enclosure="2",model="ST4000NM0023",serial="Z1Z3ABCD",slot="0"} 34
sas_physical_device_temperature_celsius{controller="0",enclosure="2",model="ST4000NM0023",serial="Z1Z3EFGH",slot="1"} 31
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected),
		"sas_exporter_tool_up", "sas_physical_device_temperature_celsius"); err != nil {
		t.Error(err)
	}
}

func TestIrcuCollectorCollectToolsMissing(t *testing.T) {
	stubRunner(t, func(_ context.Context, name string, _ ...string) ([]byte, error) {
		return nil, &exec.Error{Name: name, Err: exec.ErrNotFound}
	})

	expected := `
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="sas2ircu"} 0
sas_exporter_tool_up{tool="sas3ircu"} 0
`
	c := NewIrcuCollector("sas3ircu", "sas2ircu")
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected)); err != nil {
		t.Error(err)
	}
}

func TestIrcuCollectorCollectDisplayFailure(t *testing.T) {
	stubRunner(t, func(_ context.Context, _ string, args ...string) ([]byte, error) {
		if len(args) == 1 && args[0] == "LIST" {
			return readFixture(t, "sas3ircu_list.txt"), nil
		}
		return nil, errors.New("exit status 1")
	})

	c := NewIrcuCollector("sas3ircu", "sas2ircu")
	if got := testutil.CollectAndCount(c, "sas_controller_info"); got != 0 {
		t.Errorf("controller info series = %d, want 0", got)
	}

	expected := `
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="sas2ircu"} 1
sas_exporter_tool_up{tool="sas3ircu"} 1
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected), "sas_exporter_tool_up"); err != nil {
		t.Error(err)
	}
}

func TestIrcuCollectorCollectListFailure(t *testing.T) {
	stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
		return nil, errors.New("exit status 1")
	})

	expected := `
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="sas2ircu"} 0
sas_exporter_tool_up{tool="sas3ircu"} 0
`
	c := NewIrcuCollector("sas3ircu", "sas2ircu")
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected)); err != nil {
		t.Error(err)
	}
}

func TestScrapeSkipsUnreadableController(t *testing.T) {
	stubRunner(t, func(_ context.Context, _ string, args ...string) ([]byte, error) {
		switch {
		case len(args) == 1 && args[0] == "LIST":
			return readFixture(t, "sas2ircu_list.txt"), nil
		case args[0] == "0":
			return readFixture(t, "sas2ircu_display_0.txt"), nil
		default:
			return nil, errors.New("exit status 1")
		}
	})

	controllers, devices, err := scrape("sas2ircu")
	if err != nil {
		t.Fatalf("scrape returned error: %v", err)
	}
	if len(controllers) != 1 || controllers[0].index != "0" {
		t.Errorf("controllers = %#v, want only index 0", controllers)
	}
	if len(devices) != 2 {
		t.Errorf("devices = %d, want 2", len(devices))
	}
}
