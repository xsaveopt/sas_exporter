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
		"--json mpt list":                   "sasctl_mpt_list.json",
		"--json mpt -c 0 drive list":        "sasctl_mpt_0_drives.json",
		"--json mpt -c 1 drive list":        "sasctl_mpt_1_drives.json",
		"--json mpi3 list":                  "sasctl_mpi3_list.json",
		"--json mpi3 -c 0 drive list":       "sasctl_mpi3_0_drives.json",
		"--json mega list":                  "sasctl_mega_list.json",
		"--json mega -c 0 temperature show": "sasctl_mega_0_temperature.json",
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
			found = len(f.GetMetric()) == 2
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
sas_controller_info{bios_version="8.37.00.00",controller="1",firmware_version="16.00.12.00",pci_address="0000:81:00.0",type="SAS3008"} 1
sas_controller_info{bios_version="",controller="0",firmware_version="8.8.1.0",pci_address="0000:41:00.0",type="SAS4116"} 1
# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.
# TYPE sas_controller_temperature_celsius gauge
sas_controller_temperature_celsius{controller="0",label="Ctrl temperature",sensor="ctrl"} 47
sas_controller_temperature_celsius{controller="0",label="ROC temperature",sensor="roc"} 61
# HELP sas_exporter_tool_up 1 if the named sasctl family ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="mega"} 1
sas_exporter_tool_up{tool="mpi3"} 1
sas_exporter_tool_up{tool="mpt"} 1
# HELP sas_physical_device_info SAS physical device information, always 1.
# TYPE sas_physical_device_info gauge
sas_physical_device_info{controller="0",drive_type="SAS_HDD",enclosure="2",manufacturer="HGST",model="HUS726040ALS210",protocol="SAS",serial="K7G1ABCD",slot="0",state="RDY"} 1
sas_physical_device_info{controller="0",drive_type="NVMe_SSD",enclosure="1",manufacturer="NVMe",model="Samsung SSD 980 PRO",protocol="NVMe",serial="S5GXNX0T",slot="4",state="healthy"} 1
sas_physical_device_info{controller="0",drive_type="SATA_SSD",enclosure="2",manufacturer="ATA",model="Samsung SSD 860",protocol="SATA",serial="S3Z9NB0K",slot="1",state="OPT"} 1
# HELP sas_physical_device_temperature_celsius SAS physical device temperature in Celsius.
# TYPE sas_physical_device_temperature_celsius gauge
sas_physical_device_temperature_celsius{controller="0",enclosure="1",model="Samsung SSD 980 PRO",serial="S5GXNX0T",slot="4"} 41
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
	delete(responses, "--json mpt list")
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
	delete(responses, "--json mpt -c 0 drive list")
	delete(responses, "--json mpi3 -c 0 drive list")
	delete(responses, "--json mega -c 0 temperature show")
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
