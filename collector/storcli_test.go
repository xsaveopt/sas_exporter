package collector

import (
	"context"
	"errors"
	"os/exec"
	"slices"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func TestStorCLIControllerRe(t *testing.T) {
	tests := []struct {
		line string
		want string
	}{
		{"Controller = 0", "0"},
		{"Controller = 12", "12"},
		{"Controller=3", "3"},
		{"controller = 4", "4"},
		{"Controller   =   5", "5"},
		{"Controller Properties :", ""},
		{"Controller = none", ""},
		{"Status = Success", ""},
		{"CLI Version = 007.1017.0000.0000 May 10, 2019", ""},
		{"  Controller = 0", ""},
	}

	for _, tc := range tests {
		t.Run(tc.line, func(t *testing.T) {
			m := storCLIControllerRe.FindStringSubmatch(tc.line)
			got := ""
			if m != nil {
				got = m[1]
			}
			if got != tc.want {
				t.Errorf("match on %q = %q, want %q", tc.line, got, tc.want)
			}
		})
	}
}

func TestStorCLITempRe(t *testing.T) {
	tests := []struct {
		name       string
		line       string
		wantSensor string
		wantValue  string
	}{
		{name: "roc celsius", line: "ROC temperature(Degree Celsius) 52", wantSensor: "ROC", wantValue: "52"},
		{name: "vendor spelling variant", line: "ROC temperature(Degree Celcius) 47", wantSensor: "ROC", wantValue: "47"},
		{name: "other sensor", line: "Chip temperature(Degree Celsius) 61", wantSensor: "Chip", wantValue: "61"},
		{name: "lowercase", line: "roc temperature(degree celsius) 40", wantSensor: "roc", wantValue: "40"},
		{name: "multiple spaces inside", line: "ROC temperature(Degree  Celsius)    38", wantSensor: "ROC", wantValue: "38"},
		{name: "padded value", line: "ROC Temperature(Degree Celsius)\t\t45", wantSensor: "ROC", wantValue: "45"},
		{name: "leading whitespace is not trimmed here", line: "  ROC temperature(Degree Celsius) 52"},
		{name: "missing value", line: "ROC temperature(Degree Celsius)"},
		{name: "multi word sensor", line: "Onboard ROC temperature(Degree Celsius) 52"},
		{name: "fahrenheit", line: "ROC temperature(Degree Fahrenheit) 125"},
		{name: "unrelated", line: "Status = Success"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			m := storCLITempRe.FindStringSubmatch(tc.line)
			if tc.wantSensor == "" {
				if m != nil {
					t.Fatalf("match on %q = %q, want no match", tc.line, m)
				}
				return
			}
			if m == nil {
				t.Fatalf("no match on %q", tc.line)
			}
			if m[1] != tc.wantSensor || m[2] != tc.wantValue {
				t.Errorf("match on %q = (%q, %q), want (%q, %q)", tc.line, m[1], m[2], tc.wantSensor, tc.wantValue)
			}
		})
	}
}

func TestStorCLICollectorDescribe(t *testing.T) {
	ch := make(chan *prometheus.Desc, 4)
	NewStorCLICollector("storcli").Describe(ch)
	close(ch)

	var got []*prometheus.Desc
	for d := range ch {
		got = append(got, d)
	}
	if len(got) != 0 {
		t.Errorf("Describe sent %d descriptors, want 0 (the collector is unchecked)", len(got))
	}
}

func TestStorCLICollectorCollect(t *testing.T) {
	var gotArgs []string
	stubRunner(t, func(_ context.Context, _ string, args ...string) ([]byte, error) {
		gotArgs = args
		return readFixture(t, "storcli_temperature.txt"), nil
	})

	expected := `
# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.
# TYPE sas_controller_temperature_celsius gauge
sas_controller_temperature_celsius{controller="0",label="ROC temperature",sensor="roc"} 52
sas_controller_temperature_celsius{controller="1",label="Chip temperature",sensor="chip"} 61
sas_controller_temperature_celsius{controller="1",label="ROC temperature",sensor="roc"} 47
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="storcli"} 1
`
	c := NewStorCLICollector("storcli")
	if err := testutil.CollectAndCompare(c, strings.NewReader(expected)); err != nil {
		t.Error(err)
	}
	if want := []string{"/cALL", "show", "temperature"}; !slices.Equal(gotArgs, want) {
		t.Errorf("runner args = %q, want %q", gotArgs, want)
	}
}

func TestStorCLICollectorCollectScan(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		expected string
	}{
		{
			name:   "readings before a controller line are dropped",
			output: "ROC temperature(Degree Celsius) 52\nStatus = Success\n",
		},
		{
			name:   "indented readings are kept because the scan trims first",
			output: "Controller = 0\n    ROC temperature(Degree Celsius) 52\n",
			expected: `sas_controller_temperature_celsius{controller="0",label="ROC temperature",sensor="roc"} 52
`,
		},
		{
			name:   "empty output",
			output: "",
		},
		{
			name:   "surrounding whitespace is trimmed before matching",
			output: "   Controller = 0   \n   ROC temperature(Degree Celsius) 52   \n",
			expected: `sas_controller_temperature_celsius{controller="0",label="ROC temperature",sensor="roc"} 52
`,
		},
		{
			name:   "the last controller line wins",
			output: "Controller = 0\nController = 7\nROC temperature(Degree Celsius) 52\n",
			expected: `sas_controller_temperature_celsius{controller="7",label="ROC temperature",sensor="roc"} 52
`,
		},
		{
			name:   "sensor name is lowercased for the sensor label only",
			output: "Controller = 0\nROC temperature(Degree Celsius) 52\n",
			expected: `sas_controller_temperature_celsius{controller="0",label="ROC temperature",sensor="roc"} 52
`,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
				return []byte(tc.output), nil
			})

			expected := ""
			if tc.expected != "" {
				expected = "\n# HELP sas_controller_temperature_celsius SAS controller temperature in Celsius.\n" +
					"# TYPE sas_controller_temperature_celsius gauge\n" + tc.expected
			}

			c := NewStorCLICollector("storcli")
			if err := testutil.CollectAndCompare(c, strings.NewReader(expected),
				"sas_controller_temperature_celsius"); err != nil {
				t.Error(err)
			}
		})
	}
}

func TestStorCLICollectorCollectToolFailure(t *testing.T) {
	tests := []struct {
		name string
		err  error
	}{
		{"binary missing", &exec.Error{Name: "storcli", Err: exec.ErrNotFound}},
		{"non zero exit", errors.New("exit status 1")},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			stubRunner(t, func(_ context.Context, _ string, _ ...string) ([]byte, error) {
				return nil, tc.err
			})

			expected := `
# HELP sas_exporter_tool_up 1 if the named tool ran successfully, 0 otherwise.
# TYPE sas_exporter_tool_up gauge
sas_exporter_tool_up{tool="storcli"} 0
`
			c := NewStorCLICollector("storcli")
			if err := testutil.CollectAndCompare(c, strings.NewReader(expected)); err != nil {
				t.Error(err)
			}
		})
	}
}
