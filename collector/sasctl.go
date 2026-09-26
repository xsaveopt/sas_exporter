package collector

import (
	"encoding/json"
	"fmt"
	"log"
	"strconv"
	"strings"

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
		"1 if the named sasctl family ran successfully, 0 otherwise.",
		[]string{"tool"},
		nil,
	)
)

const (
	familyMPT  = "mpt"
	familyMPI3 = "mpi3"
	familyMega = "mega"
)

var families = []string{familyMPT, familyMPI3, familyMega}

type DriveOptions struct {
	Errors   bool
	SMART    bool
	Locate   bool
	Progress bool
}

type SasctlCollector struct {
	path   func() (string, error)
	drives DriveOptions
}

func NewSasctlCollector(path func() (string, error), drives DriveOptions) *SasctlCollector {
	return &SasctlCollector{path: path, drives: drives}
}

func (c *SasctlCollector) Describe(ch chan<- *prometheus.Desc) {
	ch <- controllerInfoDesc
	ch <- deviceInfoDesc
	ch <- deviceTempDesc
	ch <- toolUpDesc
	for _, d := range controllerDescs {
		ch <- d
	}
	for _, d := range driveDescs {
		ch <- d
	}
}

func (c *SasctlCollector) Collect(ch chan<- prometheus.Metric) {
	up := map[string]bool{}
	defer func() {
		for _, family := range families {
			reportTool(ch, family, up[family])
		}
	}()

	path, err := c.path()
	if err != nil {
		log.Printf("sas_exporter: sasctl: %v", err)
		return
	}

	var controllers []controllerRow
	if err := runJSON(path, &controllers, "controller"); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}

	for _, family := range families {
		up[family] = true
	}
	for _, ctrl := range controllers {
		family := familyOf(ctrl.Driver)
		if family == "" {
			continue
		}
		if ctrl.Error != nil {
			log.Printf("sas_exporter: sasctl controller %d: %s", ctrl.Controller, *ctrl.Error)
			up[family] = false
			continue
		}
		id := strconv.Itoa(ctrl.Controller)
		collectControllerTemperature(ch, path, id)
		ch <- prometheus.MustNewConstMetric(
			controllerInfoDesc, prometheus.GaugeValue, 1,
			id, deref(ctrl.Model), deref(ctrl.FirmwareVersion), deref(ctrl.BIOSVersion), deref(ctrl.PCIAddress),
		)
		collectControllerDetail(ch, path, id)
		collectVolumes(ch, path, id, family)
		if family == familyMega {
			collectBattery(ch, path, id)
			collectPatrol(ch, path, id)
		} else {
			collectPhys(ch, path, id)
		}
		drives := collectDrives(ch, path, id)
		collectDriveDetails(ch, path, id, family, drives, c.drives)
	}
}

func familyOf(driver string) string {
	switch driver {
	case "mpt2sas", "mpt3sas":
		return familyMPT
	case "mpi3mr":
		return familyMPI3
	case "megaraid_sas":
		return familyMega
	}
	return ""
}

func reportTool(ch chan<- prometheus.Metric, family string, up bool) {
	recordToolStatus(family, up)
	value := 0.0
	if up {
		value = 1
	}
	ch <- prometheus.MustNewConstMetric(toolUpDesc, prometheus.GaugeValue, value, family)
}

func runJSON(path string, out any, args ...string) error {
	raw, err := runTool(path, append([]string{"--json"}, args...)...)
	if len(raw) == 0 && err != nil {
		return fmt.Errorf("sasctl %s: %w", strings.Join(args, " "), err)
	}
	if jsonErr := json.Unmarshal(raw, out); jsonErr != nil {
		if err != nil {
			return fmt.Errorf("sasctl %s: %w", strings.Join(args, " "), err)
		}
		return fmt.Errorf("decoding sasctl %s: %w", strings.Join(args, " "), jsonErr)
	}
	return nil
}

type controllerRow struct {
	Controller      int     `json:"controller"`
	Driver          string  `json:"driver"`
	PCIAddress      *string `json:"pci_address"`
	Model           *string `json:"model"`
	FirmwareVersion *string `json:"firmware_version"`
	BIOSVersion     *string `json:"bios_version"`
	Error           *string `json:"error"`
}

type driveRow struct {
	Address      string  `json:"address"`
	Enclosure    *int    `json:"enclosure"`
	Slot         *int    `json:"slot"`
	State        string  `json:"state"`
	Protocol     string  `json:"protocol"`
	DriveType    *string `json:"drive_type"`
	Vendor       *string `json:"vendor"`
	Model        *string `json:"model"`
	SerialNumber *string `json:"serial_number"`
	Temperature  *struct {
		Celsius float64 `json:"celsius"`
	} `json:"temperature"`
	TemperatureCelsius *float64 `json:"temperature_celsius"`
	Error              *string  `json:"error"`
}

type driveEntry struct {
	Error  *string    `json:"error"`
	Drives []driveRow `json:"drives"`
}

type driveRef struct {
	address   string
	enclosure string
	slot      string
	protocol  string
}

func (d driveRow) ref() driveRef {
	r := driveRef{address: d.Address, protocol: d.Protocol}
	if d.Enclosure != nil && d.Slot != nil {
		r.enclosure, r.slot = strconv.Itoa(*d.Enclosure), strconv.Itoa(*d.Slot)
	} else if enc, slot, ok := strings.Cut(d.Address, ":"); ok {
		r.enclosure, r.slot = enc, slot
	} else {
		r.slot = d.Address
	}
	if r.address == "" {
		r.address = r.enclosure + ":" + r.slot
	}
	return r
}

func collectDrives(ch chan<- prometheus.Metric, path, ctrl string) []driveRef {
	var entries []driveEntry
	if err := runJSON(path, &entries, "drive", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return nil
	}
	var refs []driveRef
	for _, e := range entries {
		if e.Error != nil {
			log.Printf("sas_exporter: sasctl drive -c %s: %s", ctrl, *e.Error)
			continue
		}
		for _, d := range e.Drives {
			r := d.ref()
			if d.Error != nil {
				log.Printf("sas_exporter: sasctl drive -c %s: %s: %s", ctrl, r.address, *d.Error)
				continue
			}
			refs = append(refs, r)
			model, serial := deref(d.Model), deref(d.SerialNumber)
			ch <- prometheus.MustNewConstMetric(
				deviceInfoDesc, prometheus.GaugeValue, 1,
				ctrl, r.enclosure, r.slot, stateCode(d.State), d.Protocol,
				deref(d.DriveType), deref(d.Vendor), model, serial,
			)
			temp := d.TemperatureCelsius
			if d.Temperature != nil {
				temp = &d.Temperature.Celsius
			}
			if temp != nil {
				ch <- prometheus.MustNewConstMetric(
					deviceTempDesc, prometheus.GaugeValue, *temp,
					ctrl, r.enclosure, r.slot, model, serial,
				)
			}
		}
	}
	return refs
}

type temperatureEntry struct {
	Error             *string  `json:"error"`
	ROCCelsius        *float64 `json:"roc_celsius"`
	ControllerCelsius *float64 `json:"controller_celsius"`
	Sensors           []struct {
		Name     string   `json:"name"`
		Index    int      `json:"index"`
		Location string   `json:"location"`
		Celsius  *float64 `json:"celsius"`
	} `json:"sensors"`
}

func collectControllerTemperature(ch chan<- prometheus.Metric, path, ctrl string) {
	var entries []temperatureEntry
	if err := runJSON(path, &entries, "temperature", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, temp := range entries {
		if temp.Error != nil {
			log.Printf("sas_exporter: sasctl temperature -c %s: %s", ctrl, *temp.Error)
			continue
		}
		if temp.ROCCelsius != nil {
			ch <- prometheus.MustNewConstMetric(
				controllerTempDesc, prometheus.GaugeValue, *temp.ROCCelsius,
				ctrl, "roc", "ROC temperature",
			)
		}
		if temp.ControllerCelsius != nil {
			ch <- prometheus.MustNewConstMetric(
				controllerTempDesc, prometheus.GaugeValue, *temp.ControllerCelsius,
				ctrl, "ctrl", "Ctrl temperature",
			)
		}
		for _, sensor := range temp.Sensors {
			if sensor.Celsius == nil {
				continue
			}
			id, label := strings.ToLower(sensor.Name), sensor.Name+" temperature"
			if sensor.Name == "" {
				id, label = "sensor"+strconv.Itoa(sensor.Index), sensor.Location+" temperature"
			}
			ch <- prometheus.MustNewConstMetric(
				controllerTempDesc, prometheus.GaugeValue, *sensor.Celsius,
				ctrl, id, label,
			)
		}
	}
}

func stateCode(state string) string {
	open := strings.LastIndex(state, "(")
	if open < 0 || !strings.HasSuffix(state, ")") {
		return state
	}
	return state[open+1 : len(state)-1]
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
