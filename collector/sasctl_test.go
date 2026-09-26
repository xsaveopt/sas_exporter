package collector

import (
	"context"
	"encoding/json"
	"errors"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

const fakeSasctl = "/proc/self/fd/9"

func fixedPath(p string, err error) func() (string, error) {
	return func() (string, error) { return p, err }
}

func readFixture(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatalf("reading fixture %s: %v", name, err)
	}
	return data
}

func sasctlRunner(t *testing.T, responses map[string]string) commandRunner {
	t.Helper()
	return func(_ context.Context, name string, args ...string) ([]byte, error) {
		if name != fakeSasctl {
			t.Errorf("ran %q, want %q", name, fakeSasctl)
		}
		key := strings.Join(args, " ")
		fixture, ok := responses[key]
		if !ok {
			return nil, errors.New("unexpected command: " + key)
		}
		return readFixture(t, fixture), nil
	}
}

type combinedCollector []prometheus.Collector

func (cc combinedCollector) Describe(ch chan<- *prometheus.Desc) {
	for _, c := range cc {
		c.Describe(ch)
	}
}

func (cc combinedCollector) Collect(ch chan<- prometheus.Metric) {
	for _, c := range cc {
		c.Collect(ch)
	}
}

func newTestCollector(t *testing.T, path func() (string, error)) combinedCollector {
	t.Helper()
	return newTestCollectorWith(t, path, DriveOptions{})
}

func newTestCollectorWith(t *testing.T, path func() (string, error), drives DriveOptions) combinedCollector {
	t.Helper()
	return combinedCollector{NewHwmonCollector(filepath.Join(t.TempDir(), "absent")), NewSasctlCollector(path, drives)}
}

func fullResponses() map[string]string {
	return map[string]string{
		"--json controller":       "sasctl_controllers.json",
		"--json drive -c 0":       "sasctl_0_drives.json",
		"--json drive -c 2":       "sasctl_2_drives.json",
		"--json drive -c 3":       "sasctl_3_drives.json",
		"--json temperature -c 0": "sasctl_0_temperature.json",
		"--json temperature -c 1": "sasctl_1_temperature.json",
		"--json temperature -c 2": "sasctl_2_temperature.json",
		"--json temperature -c 3": "sasctl_3_temperature.json",
	}
}

func TestSasctlCollectorDescribe(t *testing.T) {
	c := NewSasctlCollector(fixedPath(fakeSasctl, nil), DriveOptions{})
	ch := make(chan *prometheus.Desc, 64)
	c.Describe(ch)
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
	for _, d := range slices.Concat(controllerDescs, driveDescs) {
		want = append(want, d.String())
	}
	if !slices.Equal(got, want) {
		t.Fatalf("Describe() = %v, want %v", got, want)
	}
}

func TestSasctlCollectorRegistersBesideHwmon(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, sasctlRunner(t, fullResponses()))

	reg := prometheus.NewRegistry()
	if err := reg.Register(NewHwmonCollector(t.TempDir())); err != nil {
		t.Fatal(err)
	}
	if err := reg.Register(NewSasctlCollector(fixedPath(fakeSasctl, nil), DriveOptions{})); err != nil {
		t.Fatalf("registering next to the hwmon collector: %v", err)
	}
	families, err := reg.Gather()
	if err != nil {
		t.Fatalf("gathering: %v", err)
	}
	var found bool
	for _, f := range families {
		if f.GetName() == "sas_controller_temperature_celsius" {
			found = len(f.GetMetric()) == 6
		}
	}
	if !found {
		t.Error("sas_controller_temperature_celsius missing from the combined registry")
	}
}

func TestSasctlCollectorCollect(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, sasctlRunner(t, fullResponses()))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_controller_info SAS controller information, always 1.
# TYPE sas_controller_info gauge
sas_controller_info{bios_version="7.39.02.00",controller="0",firmware_version="20.00.07.00",pci_address="0000:02:00.0",type="SAS2008"} 1
sas_controller_info{bios_version="",controller="1",firmware_version="4.740.00-8452",pci_address="0000:03:00.0",type="AVAGO MegaRAID SAS 9361-8i"} 1
sas_controller_info{bios_version="",controller="2",firmware_version="8.8.1.0",pci_address="0000:41:00.0",type="SAS4116"} 1
sas_controller_info{bios_version="8.37.00.00",controller="3",firmware_version="16.00.12.00",pci_address="0000:81:00.0",type="SAS3008"} 1
# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.
# TYPE sas_controller_temperature_celsius gauge
sas_controller_temperature_celsius{controller="0",label="Board temperature",sensor="board"} 40
sas_controller_temperature_celsius{controller="0",label="IOC temperature",sensor="ioc"} 55
sas_controller_temperature_celsius{controller="1",label="Ctrl temperature",sensor="ctrl"} 47
sas_controller_temperature_celsius{controller="1",label="ROC temperature",sensor="roc"} 61
sas_controller_temperature_celsius{controller="2",label="internal temperature",sensor="sensor0"} 52
sas_controller_temperature_celsius{controller="3",label="IOC temperature",sensor="ioc"} 62
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 1
# HELP sas_physical_device_info SAS physical device information, always 1.
# TYPE sas_physical_device_info gauge
sas_physical_device_info{controller="0",drive_type="SAS_HDD",enclosure="2",manufacturer="HGST",model="HUS726040ALS210",protocol="SAS",serial="K7G1ABCD",slot="0",state="RDY"} 1
sas_physical_device_info{controller="2",drive_type="NVMe_SSD",enclosure="1",manufacturer="NVMe",model="Samsung SSD 980 PRO",protocol="NVMe",serial="S5GXNX0T",slot="4",state="healthy"} 1
sas_physical_device_info{controller="0",drive_type="SATA_SSD",enclosure="2",manufacturer="ATA",model="Samsung SSD 860",protocol="SATA",serial="S3Z9NB0K",slot="1",state="OPT"} 1
# HELP sas_physical_device_temperature_celsius SAS physical device temperature in Celsius.
# TYPE sas_physical_device_temperature_celsius gauge
sas_physical_device_temperature_celsius{controller="2",enclosure="1",model="Samsung SSD 980 PRO",serial="S5GXNX0T",slot="4"} 41
sas_physical_device_temperature_celsius{controller="0",enclosure="2",model="HUS726040ALS210",serial="K7G1ABCD",slot="0"} 34
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want)); err != nil {
		t.Fatal(err)
	}
	if AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = true after a successful collect")
	}
}

func TestSasctlCollectorPathError(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, func(_ context.Context, name string, _ ...string) ([]byte, error) {
		t.Errorf("ran %q although sasctl is unavailable", name)
		return nil, nil
	})

	c := newTestCollector(t, fixedPath("", errors.New("not embedded")))
	want := `
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 0
sas_exporter_tool_up{tool="mpi3"} 0
sas_exporter_tool_up{tool="mpt"} 0
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
	if !AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = false with sasctl unavailable")
	}
}

func TestSasctlCollectorListFailure(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	responses := fullResponses()
	delete(responses, "--json controller")
	stubRunner(t, sasctlRunner(t, responses))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 0 {
		t.Errorf("sas_controller_info count = %d, want 0", n)
	}
	if !AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = false without a controller list")
	}
}

func TestSasctlCollectorControllerError(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	responses := fullResponses()
	responses["--json controller"] = "sasctl_controllers_mpt_down.json"
	stubRunner(t, sasctlRunner(t, responses))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 0
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 2 {
		t.Errorf("sas_controller_info count = %d, want the mpi3 and mega controllers", n)
	}
	if AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = true with mpi3 and mega still up")
	}
}

func TestSasctlCollectorSkipsFailingController(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	responses := fullResponses()
	delete(responses, "--json drive -c 0")
	delete(responses, "--json drive -c 2")
	delete(responses, "--json temperature -c 0")
	delete(responses, "--json temperature -c 1")
	delete(responses, "--json temperature -c 2")
	delete(responses, "--json temperature -c 3")
	stubRunner(t, sasctlRunner(t, responses))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 4 {
		t.Errorf("sas_controller_info count = %d, want 4", n)
	}
	if n := testutil.CollectAndCount(c, "sas_physical_device_info"); n != 0 {
		t.Errorf("sas_physical_device_info count = %d, want 0", n)
	}
	if n := testutil.CollectAndCount(c, "sas_controller_temperature_celsius"); n != 0 {
		t.Errorf("sas_controller_temperature_celsius count = %d, want 0", n)
	}
	want := `
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 1
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
}

func TestSasctlCollectorBadJSON(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
		return []byte("not json"), nil
	})

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 0 {
		t.Errorf("sas_controller_info count = %d, want 0", n)
	}
	if !AllControllerToolsDown() {
		t.Error("AllControllerToolsDown() = false when every family returned bad JSON")
	}
}

func TestStateCode(t *testing.T) {
	tests := map[string]string{
		"Ready (RDY)":       "RDY",
		"Optimal (OPT)":     "OPT",
		"Out of Sync (OSY)": "OSY",
		"Standby":           "Standby",
		"":                  "",
		"Broken (":          "Broken (",
	}
	for in, want := range tests {
		if got := stateCode(in); got != want {
			t.Errorf("stateCode(%q) = %q, want %q", in, got, want)
		}
	}
}

func optionalCommand(args []string) bool {
	key := strings.Join(args[1:], " ")
	for _, prefix := range []string{"volume ", "phy ", "battery ", "patrol ", "controller -c "} {
		if strings.HasPrefix(key, prefix) {
			return true
		}
	}
	return key == "drive -c 1"
}

type rawResponse struct {
	out string
	err error
}

func rawRunner(t *testing.T, responses map[string]rawResponse) commandRunner {
	t.Helper()
	return func(_ context.Context, name string, args ...string) ([]byte, error) {
		if name != fakeSasctl {
			t.Errorf("ran %q, want %q", name, fakeSasctl)
		}
		key := strings.Join(args, " ")
		r, ok := responses[key]
		if !ok && optionalCommand(args) {
			return nil, errors.New("no fixture for " + key)
		}
		if !ok {
			t.Errorf("unexpected command: %s", key)
			return nil, errors.New("unexpected command: " + key)
		}
		if r.out == "" {
			return nil, r.err
		}
		return []byte(r.out), r.err
	}
}

func TestRunJSON(t *testing.T) {
	exit := exitErr()
	tests := []struct {
		name       string
		out        string
		err        error
		wantErr    bool
		wantIs     error
		wantPrefix string
		wantIDs    []int
	}{
		{"clean exit", `[{"controller":0},{"controller":1}]`, nil, false, nil, "", []int{0, 1}},
		{"non-zero exit with json", `[{"controller":2,"error":"opening failed"}]`, exit, false, nil, "", []int{2}},
		{"non-zero exit without output", "", exit, true, exit, "sasctl controller: ", nil},
		{"non-zero exit with garbage", "error: no supported controllers found", exit, true, exit, "sasctl controller: ", nil},
		{"clean exit with garbage", "not json", nil, true, nil, "decoding sasctl controller: ", nil},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			stubRunner(t, rawRunner(t, map[string]rawResponse{
				"--json controller": {tc.out, tc.err},
			}))
			var rows []controllerRow
			err := runJSON(fakeSasctl, &rows, "controller")
			if tc.wantErr {
				if err == nil {
					t.Fatalf("runJSON() error = nil, want an error")
				}
				if tc.wantIs != nil && !errors.Is(err, tc.wantIs) {
					t.Errorf("runJSON() error = %v, want it to wrap %v", err, tc.wantIs)
				}
				if !strings.HasPrefix(err.Error(), tc.wantPrefix) {
					t.Errorf("runJSON() error = %q, want prefix %q", err, tc.wantPrefix)
				}
				return
			}
			if err != nil {
				t.Fatalf("runJSON() error = %v, want nil", err)
			}
			var ids []int
			for _, r := range rows {
				ids = append(ids, r.Controller)
			}
			if !slices.Equal(ids, tc.wantIDs) {
				t.Errorf("decoded controllers = %v, want %v", ids, tc.wantIDs)
			}
		})
	}
}

func TestSasctlCollectorNonZeroExitWithJSON(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	exit := exitErr()
	stubRunner(t, rawRunner(t, map[string]rawResponse{
		"--json controller":       {string(readFixture(t, "sasctl_controllers_mpt_down.json")), exit},
		"--json temperature -c 1": {string(readFixture(t, "sasctl_1_temperature.json")), nil},
		"--json temperature -c 2": {string(readFixture(t, "sasctl_2_temperature.json")), nil},
		"--json drive -c 2":       {string(readFixture(t, "sasctl_2_drives.json")), nil},
	}))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_controller_info SAS controller information, always 1.
# TYPE sas_controller_info gauge
sas_controller_info{bios_version="",controller="1",firmware_version="4.740.00-8452",pci_address="0000:03:00.0",type="AVAGO MegaRAID SAS 9361-8i"} 1
sas_controller_info{bios_version="",controller="2",firmware_version="8.8.1.0",pci_address="0000:41:00.0",type="SAS4116"} 1
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 0
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_controller_info", "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
	if n := testutil.CollectAndCount(c, "sas_physical_device_info"); n != 1 {
		t.Errorf("sas_physical_device_info count = %d, want 1", n)
	}
}

func TestSasctlCollectorDriveEntryError(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, rawRunner(t, map[string]rawResponse{
		"--json controller": {`[
  {"controller":0,"driver":"mpt3sas","pci_address":"0000:01:00.0","model":"SAS3008","firmware_version":"16.00.12.00","bios_version":"8.37.00.00","error":null}
]`, nil},
		"--json temperature -c 0": {`[{"controller":0,"driver":"mpt3sas","sensors":[]}]`, nil},
		"--json drive -c 0": {`[
  {"controller":0,"driver":"mpt3sas","error":"reading SAS device pages failed"},
  {"controller":0,"driver":"mpt3sas","drives":[{"enclosure":1,"slot":3,"state":"Ready (RDY)","protocol":"SAS","drive_type":"SAS_HDD","vendor":"VENDOR","model":"MODEL-A","serial_number":"SERIAL-A","temperature":null}]}
]`, exitErr()},
	}))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_physical_device_info SAS physical device information, always 1.
# TYPE sas_physical_device_info gauge
sas_physical_device_info{controller="0",drive_type="SAS_HDD",enclosure="1",manufacturer="VENDOR",model="MODEL-A",protocol="SAS",serial="SERIAL-A",slot="3",state="RDY"} 1
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_physical_device_info"); err != nil {
		t.Fatal(err)
	}
	if n := testutil.CollectAndCount(c, "sas_physical_device_temperature_celsius"); n != 0 {
		t.Errorf("sas_physical_device_temperature_celsius count = %d, want 0 for a drive without a temperature", n)
	}
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 1 {
		t.Errorf("sas_controller_info count = %d, want 1", n)
	}
}

func exitErr() error {
	return errors.New("exit status 1")
}

func TestSasctlCollectorTemperatureEntryError(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, rawRunner(t, map[string]rawResponse{
		"--json controller": {`[
  {"controller":0,"driver":"mpi3mr","pci_address":"0000:01:00.0","model":"SAS4116","firmware_version":"8.8.1.0","bios_version":null,"error":null},
  {"controller":1,"driver":"megaraid_sas","pci_address":"0000:02:00.0","model":"MegaRAID","firmware_version":"1.0","bios_version":null,"error":null}
]`, nil},
		"--json temperature -c 0": {`[
  {"controller":0,"driver":"mpi3mr","error":"reading IO unit page 8 failed"},
  {"controller":0,"driver":"mpi3mr","sensors":[
    {"index":0,"celsius":null,"location":"internal"},
    {"index":1,"celsius":48,"location":"external"},
    {"name":"Board","index":2,"celsius":39}
  ]}
]`, nil},
		"--json temperature -c 1": {`[{"controller":1,"driver":"megaraid_sas","error":"firmware command failed"}]`, exitErr()},
		"--json drive -c 0":       {`[{"controller":0,"driver":"mpi3mr","drives":[]}]`, nil},
	}))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.
# TYPE sas_controller_temperature_celsius gauge
sas_controller_temperature_celsius{controller="0",label="Board temperature",sensor="board"} 39
sas_controller_temperature_celsius{controller="0",label="external temperature",sensor="sensor1"} 48
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 1
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_controller_temperature_celsius", "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 2 {
		t.Errorf("sas_controller_info count = %d, want the mpi3 and mega controllers", n)
	}
}

func TestSasctlCollectorUnknownDriver(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, rawRunner(t, map[string]rawResponse{
		"--json controller": {`[
  {"controller":0,"driver":"aacraid","pci_address":"0000:01:00.0","model":"Other","firmware_version":"1.0","bios_version":null,"error":null},
  {"controller":1,"driver":"","pci_address":null,"model":null,"firmware_version":null,"bios_version":null,"error":"opening failed"}
]`, nil},
	}))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 1
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_exporter_tool_up"); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"sas_controller_info", "sas_physical_device_info", "sas_controller_temperature_celsius"} {
		if n := testutil.CollectAndCount(c, name); n != 0 {
			t.Errorf("%s count = %d, want 0 for an unknown driver", name, n)
		}
	}
}

func TestFamilyOf(t *testing.T) {
	tests := map[string]string{
		"mpt2sas":      familyMPT,
		"mpt3sas":      familyMPT,
		"mpi3mr":       familyMPI3,
		"megaraid_sas": familyMega,
		"aacraid":      "",
		"":             "",
		"MPT3SAS":      "",
	}
	for in, want := range tests {
		if got := familyOf(in); got != want {
			t.Errorf("familyOf(%q) = %q, want %q", in, got, want)
		}
	}
}

func extendedResponses() map[string]string {
	responses := fullResponses()
	maps.Copy(responses, map[string]string{
		"--json controller -c 0":        "sasctl_0_controller.json",
		"--json controller -c 1":        "sasctl_1_controller.json",
		"--json controller -c 2":        "sasctl_2_controller.json",
		"--json phy -c 0":               "sasctl_0_phy.json",
		"--json phy errors -c 0":        "sasctl_0_phy_errors.json",
		"--json volume -c 0":            "sasctl_0_volumes.json",
		"--json drive -c 1":             "sasctl_1_drives.json",
		"--json volume -c 1":            "sasctl_1_volumes.json",
		"--json volume 0 progress -c 1": "sasctl_1_volume_0_progress.json",
		"--json battery -c 1":           "sasctl_1_battery.json",
		"--json patrol -c 1":            "sasctl_1_patrol.json",
		"--json drive 252:0 -c 1":       "sasctl_1_drive_252_0.json",
		"--json drive 252:0 smart -c 1": "sasctl_1_drive_252_0_smart.json",
		"--json drive 1:4 smart -c 2":   "sasctl_2_drive_1_4_smart.json",
	})
	return responses
}

func recordingRunner(t *testing.T, responses map[string]string, ran *[]string) commandRunner {
	t.Helper()
	inner := sasctlRunner(t, responses)
	return func(ctx context.Context, name string, args ...string) ([]byte, error) {
		*ran = append(*ran, strings.Join(args, " "))
		return inner(ctx, name, args...)
	}
}

func TestSasctlCollectorControllerData(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	var ran []string
	stubRunner(t, recordingRunner(t, extendedResponses(), &ran))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_battery_charge_percent Relative charge of the battery in percent.
# TYPE sas_battery_charge_percent gauge
sas_battery_charge_percent{controller="1"} 96
# HELP sas_battery_current_amps Battery current in amps, negative while discharging.
# TYPE sas_battery_current_amps gauge
sas_battery_current_amps{controller="1"} -0.012
# HELP sas_battery_cycles_total Charge cycles the battery has been through.
# TYPE sas_battery_cycles_total counter
sas_battery_cycles_total{controller="1"} 14
# HELP sas_battery_design_capacity_mah Design capacity of the battery in mAh.
# TYPE sas_battery_design_capacity_mah gauge
sas_battery_design_capacity_mah{controller="1"} 1350
# HELP sas_battery_full_charge_capacity_mah Battery capacity when fully charged in mAh.
# TYPE sas_battery_full_charge_capacity_mah gauge
sas_battery_full_charge_capacity_mah{controller="1"} 1200
# HELP sas_battery_health_good 1 if the battery reports a good state of health, 0 otherwise.
# TYPE sas_battery_health_good gauge
sas_battery_health_good{controller="1"} 1
# HELP sas_battery_remaining_capacity_mah Remaining battery capacity in mAh.
# TYPE sas_battery_remaining_capacity_mah gauge
sas_battery_remaining_capacity_mah{controller="1"} 1100
# HELP sas_battery_temperature_celsius Battery temperature in Celsius.
# TYPE sas_battery_temperature_celsius gauge
sas_battery_temperature_celsius{controller="1"} 31
# HELP sas_battery_voltage_volts Battery voltage in volts.
# TYPE sas_battery_voltage_volts gauge
sas_battery_voltage_volts{controller="1"} 4.012
# HELP sas_controller_alarm_present 1 if the controller has an alarm, 0 otherwise.
# TYPE sas_controller_alarm_present gauge
sas_controller_alarm_present{controller="1"} 0
# HELP sas_controller_battery_present 1 if the controller has a BBU or CacheVault, 0 otherwise.
# TYPE sas_controller_battery_present gauge
sas_controller_battery_present{controller="1"} 1
# HELP sas_controller_devices Devices the controller sees, by type.
# TYPE sas_controller_devices gauge
sas_controller_devices{controller="2",type="drives"} 1
sas_controller_devices{controller="2",type="enclosures"} 1
sas_controller_devices{controller="2",type="expanders"} 1
sas_controller_devices{controller="2",type="pcie"} 1
sas_controller_devices{controller="2",type="sas_sata"} 3
sas_controller_devices{controller="2",type="virtual_disks"} 0
# HELP sas_controller_drives_failed Drives the controller reports as failed.
# TYPE sas_controller_drives_failed gauge
sas_controller_drives_failed{controller="1"} 0
# HELP sas_controller_drives_predictive_failure Drives the controller reports with a predictive failure.
# TYPE sas_controller_drives_predictive_failure gauge
sas_controller_drives_predictive_failure{controller="1"} 1
# HELP sas_controller_ioc_exceptions Raw IOC exceptions bitmask reported by the controller, 0 means none.
# TYPE sas_controller_ioc_exceptions gauge
sas_controller_ioc_exceptions{controller="0"} 0
sas_controller_ioc_exceptions{controller="2"} 4
# HELP sas_controller_memory_errors_total Controller memory errors, by type.
# TYPE sas_controller_memory_errors_total counter
sas_controller_memory_errors_total{controller="1",type="correctable"} 3
sas_controller_memory_errors_total{controller="1",type="uncorrectable"} 0
# HELP sas_controller_volumes_degraded Volumes the controller reports as degraded.
# TYPE sas_controller_volumes_degraded gauge
sas_controller_volumes_degraded{controller="1"} 1
# HELP sas_controller_volumes_offline Volumes the controller reports as offline.
# TYPE sas_controller_volumes_offline gauge
sas_controller_volumes_offline{controller="1"} 0
# HELP sas_patrol_drives_done Drives the running patrol read has finished.
# TYPE sas_patrol_drives_done gauge
sas_patrol_drives_done{controller="1"} 1
# HELP sas_patrol_info Patrol read state and mode, always 1.
# TYPE sas_patrol_info gauge
sas_patrol_info{controller="1",mode="auto",state="active"} 1
# HELP sas_patrol_iterations_total Completed patrol read iterations.
# TYPE sas_patrol_iterations_total counter
sas_patrol_iterations_total{controller="1"} 42
# HELP sas_patrol_next_run_seconds Seconds until the next scheduled patrol read.
# TYPE sas_patrol_next_run_seconds gauge
sas_patrol_next_run_seconds{controller="1"} 23
# HELP sas_phy_enabled 1 if the controller phy is enabled, 0 otherwise.
# TYPE sas_phy_enabled gauge
sas_phy_enabled{controller="0",phy="0"} 1
sas_phy_enabled{controller="0",phy="1"} 1
# HELP sas_phy_errors_total Controller phy link error counters, by type.
# TYPE sas_phy_errors_total counter
sas_phy_errors_total{controller="0",phy="0",type="invalid_dword"} 12
sas_phy_errors_total{controller="0",phy="0",type="loss_dword_sync"} 1
sas_phy_errors_total{controller="0",phy="0",type="phy_reset_problem"} 0
sas_phy_errors_total{controller="0",phy="0",type="running_disparity"} 3
# HELP sas_phy_link_rate_gbps Negotiated link rate of the controller phy in Gb/s, 0 when there is no link.
# TYPE sas_phy_link_rate_gbps gauge
sas_phy_link_rate_gbps{controller="0",phy="0"} 6
sas_phy_link_rate_gbps{controller="0",phy="1"} 0
# HELP sas_phy_max_link_rate_gbps Highest link rate the controller phy supports in Gb/s.
# TYPE sas_phy_max_link_rate_gbps gauge
sas_phy_max_link_rate_gbps{controller="0",phy="0"} 6
# HELP sas_volume_info RAID volume information, always 1.
# TYPE sas_volume_info gauge
sas_volume_info{controller="0",name="data",raid_level="RAID1",state="DGD",volume="323"} 1
sas_volume_info{controller="1",name="mirror",raid_level="RAID1",state="Dgrd",volume="0"} 1
# HELP sas_volume_operation_progress_percent Progress of a running volume operation in percent.
# TYPE sas_volume_operation_progress_percent gauge
sas_volume_operation_progress_percent{controller="0",operation="consistency_check",volume="323"} 50
sas_volume_operation_progress_percent{controller="1",operation="consistency_check",volume="0"} 25
`
	names := []string{}
	for _, d := range controllerDescs {
		names = append(names, descName(d))
	}
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), names...); err != nil {
		t.Fatal(err)
	}
	for _, key := range ran {
		if strings.HasPrefix(key, "--json drive ") && !strings.HasPrefix(key, "--json drive -c ") {
			t.Errorf("ran per-drive command %q with every drive option off", key)
		}
	}
}

func TestSasctlCollectorMegaDrives(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, sasctlRunner(t, extendedResponses()))

	c := newTestCollector(t, fixedPath(fakeSasctl, nil))
	want := `
# HELP sas_physical_device_info SAS physical device information, always 1.
# TYPE sas_physical_device_info gauge
sas_physical_device_info{controller="0",drive_type="SAS_HDD",enclosure="2",manufacturer="HGST",model="HUS726040ALS210",protocol="SAS",serial="K7G1ABCD",slot="0",state="RDY"} 1
sas_physical_device_info{controller="0",drive_type="SATA_SSD",enclosure="2",manufacturer="ATA",model="Samsung SSD 860",protocol="SATA",serial="S3Z9NB0K",slot="1",state="OPT"} 1
sas_physical_device_info{controller="1",drive_type="",enclosure="252",manufacturer="",model="SEAGATE ST4000NM0025",protocol="",serial="",slot="0",state="Onln"} 1
sas_physical_device_info{controller="2",drive_type="NVMe_SSD",enclosure="1",manufacturer="NVMe",model="Samsung SSD 980 PRO",protocol="NVMe",serial="S5GXNX0T",slot="4",state="healthy"} 1
# HELP sas_physical_device_temperature_celsius SAS physical device temperature in Celsius.
# TYPE sas_physical_device_temperature_celsius gauge
sas_physical_device_temperature_celsius{controller="0",enclosure="2",model="HUS726040ALS210",serial="K7G1ABCD",slot="0"} 34
sas_physical_device_temperature_celsius{controller="1",enclosure="252",model="SEAGATE ST4000NM0025",serial="",slot="0"} 30
sas_physical_device_temperature_celsius{controller="2",enclosure="1",model="Samsung SSD 980 PRO",serial="S5GXNX0T",slot="4"} 41
`
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), "sas_physical_device_info", "sas_physical_device_temperature_celsius"); err != nil {
		t.Fatal(err)
	}
}

func TestSasctlCollectorDriveOptions(t *testing.T) {
	resetToolStatus()
	t.Cleanup(resetToolStatus)
	stubRunner(t, sasctlRunner(t, extendedResponses()))

	c := newTestCollectorWith(t, fixedPath(fakeSasctl, nil), DriveOptions{Errors: true, SMART: true, Locate: true, Progress: true})
	want := `
# HELP sas_physical_device_ata_smart_attribute_raw Raw value of an ATA SMART attribute.
# TYPE sas_physical_device_ata_smart_attribute_raw gauge
sas_physical_device_ata_smart_attribute_raw{attribute="194",controller="1",enclosure="252",slot="0"} 30
sas_physical_device_ata_smart_attribute_raw{attribute="5",controller="1",enclosure="252",slot="0"} 12
# HELP sas_physical_device_ata_smart_attribute_threshold Failure threshold of an ATA SMART attribute.
# TYPE sas_physical_device_ata_smart_attribute_threshold gauge
sas_physical_device_ata_smart_attribute_threshold{attribute="5",controller="1",enclosure="252",slot="0"} 10
# HELP sas_physical_device_ata_smart_attribute_value Normalized value of an ATA SMART attribute.
# TYPE sas_physical_device_ata_smart_attribute_value gauge
sas_physical_device_ata_smart_attribute_value{attribute="194",controller="1",enclosure="252",slot="0"} 70
sas_physical_device_ata_smart_attribute_value{attribute="5",controller="1",enclosure="252",slot="0"} 100
# HELP sas_physical_device_ata_smart_attribute_worst Worst normalized value of an ATA SMART attribute.
# TYPE sas_physical_device_ata_smart_attribute_worst gauge
sas_physical_device_ata_smart_attribute_worst{attribute="194",controller="1",enclosure="252",slot="0"} 55
sas_physical_device_ata_smart_attribute_worst{attribute="5",controller="1",enclosure="252",slot="0"} 100
# HELP sas_physical_device_bbm_errors_total Bad block management errors the controller counted for the drive.
# TYPE sas_physical_device_bbm_errors_total counter
sas_physical_device_bbm_errors_total{controller="1",enclosure="252",slot="0"} 7
# HELP sas_physical_device_locate_active 1 if the locate LED of the drive is on, 0 otherwise.
# TYPE sas_physical_device_locate_active gauge
sas_physical_device_locate_active{controller="1",enclosure="252",slot="0"} 1
# HELP sas_physical_device_media_errors_total Media errors the controller counted for the drive.
# TYPE sas_physical_device_media_errors_total counter
sas_physical_device_media_errors_total{controller="1",enclosure="252",slot="0"} 3
# HELP sas_physical_device_nvme_available_spare_percent Remaining spare capacity of the NVMe drive in percent.
# TYPE sas_physical_device_nvme_available_spare_percent gauge
sas_physical_device_nvme_available_spare_percent{controller="2",enclosure="1",slot="4"} 8
# HELP sas_physical_device_nvme_critical_warning Raw NVMe critical warning bitmask, 0 means none.
# TYPE sas_physical_device_nvme_critical_warning gauge
sas_physical_device_nvme_critical_warning{controller="2",enclosure="1",slot="4"} 1
# HELP sas_physical_device_nvme_error_log_entries_total Error log entries of the NVMe drive.
# TYPE sas_physical_device_nvme_error_log_entries_total counter
sas_physical_device_nvme_error_log_entries_total{controller="2",enclosure="1",slot="4"} 7
# HELP sas_physical_device_nvme_media_errors_total Media and data integrity errors of the NVMe drive.
# TYPE sas_physical_device_nvme_media_errors_total counter
sas_physical_device_nvme_media_errors_total{controller="2",enclosure="1",slot="4"} 0
# HELP sas_physical_device_nvme_percentage_used Estimated share of the NVMe drive endurance that is used, in percent.
# TYPE sas_physical_device_nvme_percentage_used gauge
sas_physical_device_nvme_percentage_used{controller="2",enclosure="1",slot="4"} 12
# HELP sas_physical_device_nvme_power_cycles_total Power cycles of the NVMe drive.
# TYPE sas_physical_device_nvme_power_cycles_total counter
sas_physical_device_nvme_power_cycles_total{controller="2",enclosure="1",slot="4"} 42
# HELP sas_physical_device_nvme_power_on_hours_total Hours the NVMe drive has been powered on.
# TYPE sas_physical_device_nvme_power_on_hours_total counter
sas_physical_device_nvme_power_on_hours_total{controller="2",enclosure="1",slot="4"} 1234
# HELP sas_physical_device_nvme_unsafe_shutdowns_total Unsafe shutdowns of the NVMe drive.
# TYPE sas_physical_device_nvme_unsafe_shutdowns_total counter
sas_physical_device_nvme_unsafe_shutdowns_total{controller="2",enclosure="1",slot="4"} 3
# HELP sas_physical_device_operation_progress_percent Progress of a running drive operation in percent.
# TYPE sas_physical_device_operation_progress_percent gauge
sas_physical_device_operation_progress_percent{controller="1",enclosure="252",operation="rebuild",slot="0"} 50
# HELP sas_physical_device_other_errors_total Other errors the controller counted for the drive.
# TYPE sas_physical_device_other_errors_total counter
sas_physical_device_other_errors_total{controller="1",enclosure="252",slot="0"} 5
# HELP sas_physical_device_predictive_failures_total Predictive failures the controller counted for the drive.
# TYPE sas_physical_device_predictive_failures_total counter
sas_physical_device_predictive_failures_total{controller="1",enclosure="252",slot="0"} 1
# HELP sas_physical_device_smart_healthy 1 if the drive SMART status reports no failure, 0 otherwise.
# TYPE sas_physical_device_smart_healthy gauge
sas_physical_device_smart_healthy{controller="1",enclosure="252",slot="0"} 1
sas_physical_device_smart_healthy{controller="2",enclosure="1",slot="4"} 0
`
	names := []string{}
	for _, d := range driveDescs {
		names = append(names, descName(d))
	}
	if err := testutil.CollectAndCompare(c, strings.NewReader(want), names...); err != nil {
		t.Fatal(err)
	}
}

func TestSmartHealthy(t *testing.T) {
	tests := []struct {
		name  string
		entry string
		want  bool
	}{
		{"mpi3 healthy", `{"healthy":true}`, true},
		{"mpi3 unhealthy", `{"healthy":false}`, false},
		{"mega smart alert", `{"smart_alert":true}`, false},
		{"mega predicted failure", `{"smart_alert":false,"informational_exceptions":{"failure_predicted":true}}`, false},
		{"mega ata failed", `{"smart_alert":false,"ata_smart":{"health":"FAILED","attributes":[]}}`, false},
		{"mega clean sas", `{"smart_alert":false,"informational_exceptions":{"failure_predicted":false}}`, true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var e smartEntry
			if err := json.Unmarshal([]byte(tc.entry), &e); err != nil {
				t.Fatal(err)
			}
			if got := e.healthy(); got != tc.want {
				t.Errorf("healthy() = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestLinkRateGbps(t *testing.T) {
	rate := func(s string) *string { return &s }
	tests := []struct {
		in     *string
		want   float64
		wantOK bool
	}{
		{rate("12.0 Gb/s"), 12, true},
		{rate("22.5 Gb/s"), 22.5, true},
		{rate("negotiation failed"), 0, true},
		{rate("disabled"), 0, true},
		{nil, 0, false},
	}
	for _, tc := range tests {
		got, ok := linkRateGbps(tc.in)
		if got != tc.want || ok != tc.wantOK {
			t.Errorf("linkRateGbps(%v) = %v, %v, want %v, %v", tc.in, got, ok, tc.want, tc.wantOK)
		}
	}
}

func descName(d *prometheus.Desc) string {
	s := d.String()
	start := strings.Index(s, `fqName: "`) + len(`fqName: "`)
	return s[start : start+strings.Index(s[start:], `"`)]
}
