package collector

import (
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func mkdirAll(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(path, 0o755); err != nil {
		t.Fatalf("creating dir: %v", err)
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	mkdirAll(t, filepath.Dir(path))
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("writing file: %v", err)
	}
}

func symlink(t *testing.T, target, path string) {
	t.Helper()
	mkdirAll(t, filepath.Dir(path))
	if err := os.Symlink(target, path); err != nil {
		t.Fatalf("creating symlink: %v", err)
	}
}

func fakeSysfs(t *testing.T) (root, hwmonRoot string) {
	t.Helper()
	root = t.TempDir()
	hwmonRoot = filepath.Join(root, "sys", "class", "hwmon")
	mkdirAll(t, hwmonRoot)
	return root, hwmonRoot
}

func addHwmon(t *testing.T, root, hwmonRoot, name, pciAddress, driver string) string {
	t.Helper()
	hwmonPath := filepath.Join(hwmonRoot, name)
	mkdirAll(t, hwmonPath)
	if pciAddress == "" {
		return hwmonPath
	}

	devicePath := filepath.Join(root, "sys", "devices", "pci0000:00", "0000:00:01.0", pciAddress)
	mkdirAll(t, devicePath)
	symlink(t, devicePath, filepath.Join(hwmonPath, "device"))
	if driver != "" {
		symlink(t, filepath.Join("..", "..", "..", "bus", "pci", "drivers", driver), filepath.Join(devicePath, "driver"))
	}
	return hwmonPath
}

func TestIsSASDriver(t *testing.T) {
	tests := []struct {
		name       string
		pciAddress string
		driver     string
		want       bool
	}{
		{name: "mpt3sas", pciAddress: "0000:03:00.0", driver: "mpt3sas", want: true},
		{name: "mpt2sas", pciAddress: "0000:04:00.0", driver: "mpt2sas", want: true},
		{name: "unrelated driver", pciAddress: "0000:05:00.0", driver: "coretemp"},
		{name: "driver name is matched whole", pciAddress: "0000:06:00.0", driver: "mpt3sas_old"},
		{name: "no driver link", pciAddress: "0000:07:00.0"},
		{name: "no device link"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			root, hwmonRoot := fakeSysfs(t)
			hwmonPath := addHwmon(t, root, hwmonRoot, "hwmon0", tc.pciAddress, tc.driver)
			if got := isSASDriver(hwmonPath); got != tc.want {
				t.Errorf("isSASDriver = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestIsSASDriverRejectsRegularFile(t *testing.T) {
	root, hwmonRoot := fakeSysfs(t)
	hwmonPath := addHwmon(t, root, hwmonRoot, "hwmon0", "0000:03:00.0", "")
	writeFile(t, filepath.Join(root, "sys", "devices", "pci0000:00", "0000:00:01.0", "0000:03:00.0", "driver"), "mpt3sas")

	if isSASDriver(hwmonPath) {
		t.Error("isSASDriver = true for a regular file, want false")
	}
}

func TestHwmonPCIAddress(t *testing.T) {
	root, hwmonRoot := fakeSysfs(t)

	withDevice := addHwmon(t, root, hwmonRoot, "hwmon0", "0000:03:00.0", "mpt3sas")
	if got := hwmonPCIAddress(withDevice); got != "0000:03:00.0" {
		t.Errorf("hwmonPCIAddress = %q, want %q", got, "0000:03:00.0")
	}

	withoutDevice := addHwmon(t, root, hwmonRoot, "hwmon1", "", "")
	if got := hwmonPCIAddress(withoutDevice); got != "" {
		t.Errorf("hwmonPCIAddress = %q, want empty when there is no device link", got)
	}

	dangling := filepath.Join(hwmonRoot, "hwmon2")
	symlink(t, filepath.Join(root, "sys", "devices", "missing"), filepath.Join(dangling, "device"))
	if got := hwmonPCIAddress(dangling); got != "" {
		t.Errorf("hwmonPCIAddress = %q, want empty for a dangling device link", got)
	}
}

func TestReadTempInputs(t *testing.T) {
	root, hwmonRoot := fakeSysfs(t)
	hwmonPath := addHwmon(t, root, hwmonRoot, "hwmon0", "0000:03:00.0", "mpt3sas")

	writeFile(t, filepath.Join(hwmonPath, "temp1_input"), "52000\n")
	writeFile(t, filepath.Join(hwmonPath, "temp1_label"), "ioc0\n")
	writeFile(t, filepath.Join(hwmonPath, "temp2_input"), "  48500  ")
	writeFile(t, filepath.Join(hwmonPath, "temp3_input"), "N/A\n")
	writeFile(t, filepath.Join(hwmonPath, "temp3_label"), "broken\n")
	writeFile(t, filepath.Join(hwmonPath, "temp4_max"), "95000\n")
	writeFile(t, filepath.Join(hwmonPath, "in0_input"), "1200\n")
	writeFile(t, filepath.Join(hwmonPath, "name"), "mpt3sas\n")
	mkdirAll(t, filepath.Join(hwmonPath, "temp9_input"))

	got, err := readTempInputs(hwmonPath)
	if err != nil {
		t.Fatalf("readTempInputs returned error: %v", err)
	}

	want := []tempReading{
		{sensor: "temp1", label: "ioc0", celsius: 52},
		{sensor: "temp2", label: "", celsius: 48.5},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("readTempInputs = %#v, want %#v", got, want)
	}
}

func TestReadTempInputsEmptyDir(t *testing.T) {
	_, hwmonRoot := fakeSysfs(t)
	hwmonPath := filepath.Join(hwmonRoot, "hwmon0")
	mkdirAll(t, hwmonPath)

	got, err := readTempInputs(hwmonPath)
	if err != nil {
		t.Fatalf("readTempInputs returned error: %v", err)
	}
	if got != nil {
		t.Errorf("readTempInputs = %#v, want none", got)
	}
}

func TestReadTempInputsMissingDir(t *testing.T) {
	_, hwmonRoot := fakeSysfs(t)

	_, err := readTempInputs(filepath.Join(hwmonRoot, "absent"))
	if err == nil {
		t.Fatal("readTempInputs returned no error for a missing directory")
	}
	if !strings.Contains(err.Error(), "reading dir") {
		t.Errorf("error = %v, want it to mention reading the dir", err)
	}
	if !errors.Is(err, os.ErrNotExist) {
		t.Errorf("error = %v, want a wrapped os.ErrNotExist", err)
	}
}

func TestHwmonCollectorDescribe(t *testing.T) {
	ch := make(chan *prometheus.Desc, 4)
	NewHwmonCollector("/sys/class/hwmon").Describe(ch)
	close(ch)

	var got []string
	for d := range ch {
		got = append(got, d.String())
	}
	if want := []string{controllerTempDesc.String()}; !slices.Equal(got, want) {
		t.Errorf("Describe sent %v, want %v", got, want)
	}
}

func TestHwmonCollectorCollect(t *testing.T) {
	root, hwmonRoot := fakeSysfs(t)

	sas3 := addHwmon(t, root, hwmonRoot, "hwmon0", "0000:03:00.0", "mpt3sas")
	writeFile(t, filepath.Join(sas3, "temp1_input"), "52000\n")
	writeFile(t, filepath.Join(sas3, "temp1_label"), "ioc0\n")
	writeFile(t, filepath.Join(sas3, "temp2_input"), "61250\n")

	sas2 := addHwmon(t, root, hwmonRoot, "hwmon1", "0000:81:00.0", "mpt2sas")
	writeFile(t, filepath.Join(sas2, "temp1_input"), "48000\n")

	other := addHwmon(t, root, hwmonRoot, "hwmon2", "0000:00:18.3", "k10temp")
	writeFile(t, filepath.Join(other, "temp1_input"), "70000\n")

	bare := addHwmon(t, root, hwmonRoot, "hwmon3", "", "")
	writeFile(t, filepath.Join(bare, "temp1_input"), "31000\n")

	expected := `
# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.
# TYPE sas_controller_temperature_celsius gauge
sas_controller_temperature_celsius{controller="0000:03:00.0",label="ioc0",sensor="temp1"} 52
sas_controller_temperature_celsius{controller="0000:03:00.0",label="",sensor="temp2"} 61.25
sas_controller_temperature_celsius{controller="0000:81:00.0",label="",sensor="temp1"} 48
`
	c := NewHwmonCollector(hwmonRoot)
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected)); err != nil {
		t.Error(err)
	}
}

func TestHwmonCollectorCollectMissingRoot(t *testing.T) {
	_, hwmonRoot := fakeSysfs(t)

	c := NewHwmonCollector(filepath.Join(hwmonRoot, "absent"))
	if got := testutil.CollectAndCount(c); got != 0 {
		t.Errorf("collected %d series from a missing root, want 0", got)
	}
}

func TestHwmonCollectorCollectUnreadableDir(t *testing.T) {
	root, hwmonRoot := fakeSysfs(t)

	hwmonPath := filepath.Join(hwmonRoot, "hwmon0")
	mkdirAll(t, hwmonPath)
	devicePath := filepath.Join(root, "sys", "devices", "pci0000:00", "0000:00:01.0", "0000:03:00.0")
	mkdirAll(t, devicePath)
	symlink(t, filepath.Join("..", "..", "..", "bus", "pci", "drivers", "mpt3sas"), filepath.Join(devicePath, "driver"))
	symlink(t, devicePath, filepath.Join(hwmonPath, "device"))
	writeFile(t, filepath.Join(hwmonPath, "temp1_input"), "52000\n")
	if err := os.Chmod(hwmonPath, 0o111); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { _ = os.Chmod(hwmonPath, 0o755) })

	c := NewHwmonCollector(hwmonRoot)
	if got := testutil.CollectAndCount(c); got != 0 {
		t.Errorf("collected %d series from an unreadable hwmon dir, want 0", got)
	}
}
