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

type SasctlCollector struct {
	path func() (string, error)
}

func NewSasctlCollector(path func() (string, error)) *SasctlCollector {
	return &SasctlCollector{path: path}
}

func (c *SasctlCollector) Describe(ch chan<- *prometheus.Desc) {
	ch <- controllerInfoDesc
	ch <- deviceInfoDesc
	ch <- deviceTempDesc
	ch <- toolUpDesc
}

func (c *SasctlCollector) Collect(ch chan<- prometheus.Metric) {
	path, err := c.path()
	if err != nil {
		log.Printf("sas_exporter: sasctl: %v", err)
		for _, family := range families {
			reportTool(ch, family, false)
		}
		return
	}

	for _, family := range []string{familyMPT, familyMPI3} {
		err = collectHBA(ch, path, family)
		if err != nil {
			log.Printf("sas_exporter: sasctl %s: %v", family, err)
		}
		reportTool(ch, family, err == nil)
	}

	err = collectMega(ch, path)
	if err != nil {
		log.Printf("sas_exporter: sasctl mega: %v", err)
	}
	reportTool(ch, familyMega, err == nil)
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
	if err != nil {
		return fmt.Errorf("sasctl %s: %w", strings.Join(args, " "), err)
	}
	if err := json.Unmarshal(raw, out); err != nil {
		return fmt.Errorf("decoding sasctl %s: %w", strings.Join(args, " "), err)
	}
	return nil
}

type adapterList struct {
	Adapters []struct {
		Index           int     `json:"index"`
		Chip            string  `json:"chip"`
		PCIAddress      string  `json:"pci_address"`
		FirmwareVersion *string `json:"firmware_version"`
		BIOSVersion     *string `json:"bios_version"`
	} `json:"adapters"`
}

type driveList struct {
	Drives []struct {
		Enclosure    int     `json:"enclosure"`
		Slot         int     `json:"slot"`
		State        string  `json:"state"`
		Protocol     string  `json:"protocol"`
		DriveType    *string `json:"drive_type"`
		Vendor       *string `json:"vendor"`
		Model        *string `json:"model"`
		SerialNumber *string `json:"serial_number"`
		Temperature  *struct {
			Celsius float64 `json:"celsius"`
		} `json:"temperature"`
	} `json:"drives"`
}

func collectHBA(ch chan<- prometheus.Metric, path, family string) error {
	var adapters adapterList
	if err := runJSON(path, &adapters, family, "list"); err != nil {
		return err
	}
	for _, a := range adapters.Adapters {
		ctrl := strconv.Itoa(a.Index)
		ch <- prometheus.MustNewConstMetric(
			controllerInfoDesc, prometheus.GaugeValue, 1,
			ctrl, a.Chip, deref(a.FirmwareVersion), deref(a.BIOSVersion), a.PCIAddress,
		)

		var drives driveList
		if err := runJSON(path, &drives, family, "-c", ctrl, "drive", "list"); err != nil {
			log.Printf("sas_exporter: %v", err)
			continue
		}
		for _, d := range drives.Drives {
			enclosure, slot := strconv.Itoa(d.Enclosure), strconv.Itoa(d.Slot)
			model, serial := deref(d.Model), deref(d.SerialNumber)
			ch <- prometheus.MustNewConstMetric(
				deviceInfoDesc, prometheus.GaugeValue, 1,
				ctrl, enclosure, slot, stateCode(d.State), d.Protocol,
				deref(d.DriveType), deref(d.Vendor), model, serial,
			)
			if d.Temperature != nil {
				ch <- prometheus.MustNewConstMetric(
					deviceTempDesc, prometheus.GaugeValue, d.Temperature.Celsius,
					ctrl, enclosure, slot, model, serial,
				)
			}
		}
	}
	return nil
}

type megaControllerList struct {
	Controllers []struct {
		Index int `json:"index"`
	} `json:"controllers"`
}

type megaTemperature struct {
	ROCCelsius        *float64 `json:"roc_celsius"`
	ControllerCelsius *float64 `json:"controller_celsius"`
}

func collectMega(ch chan<- prometheus.Metric, path string) error {
	var controllers megaControllerList
	if err := runJSON(path, &controllers, familyMega, "list"); err != nil {
		return err
	}
	for _, c := range controllers.Controllers {
		ctrl := strconv.Itoa(c.Index)
		var temp megaTemperature
		if err := runJSON(path, &temp, familyMega, "-c", ctrl, "temperature", "show"); err != nil {
			log.Printf("sas_exporter: %v", err)
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
	}
	return nil
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
