package collector

import (
	"context"
	"errors"
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
	return combinedCollector{NewHwmonCollector(filepath.Join(t.TempDir(), "absent")), NewSasctlCollector(path)}
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
	c := NewSasctlCollector(fixedPath(fakeSasctl, nil))
	ch := make(chan *prometheus.Desc, 10)
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
	if err := reg.Register(NewSasctlCollector(fixedPath(fakeSasctl, nil))); err != nil {
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
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 1 {
		t.Errorf("sas_controller_info count = %d, want only the mpi3 controller", n)
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
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 3 {
		t.Errorf("sas_controller_info count = %d, want 3", n)
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
	if n := testutil.CollectAndCount(c, "sas_controller_info"); n != 1 {
		t.Errorf("sas_controller_info count = %d, want only the mpi3 controller", n)
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
