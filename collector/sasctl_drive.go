package collector

import (
	"log"
	"strconv"

	"github.com/prometheus/client_golang/prometheus"
)

var driveLabels = []string{"controller", "enclosure", "slot"}

func driveDesc(name, help string, extra ...string) *prometheus.Desc {
	return newDesc("physical_device", name, help, append(append([]string{}, driveLabels...), extra...)...)
}

var (
	driveMediaErrorsDesc = driveDesc("media_errors_total",
		"Media errors the controller counted for the drive.")
	driveOtherErrorsDesc = driveDesc("other_errors_total",
		"Other errors the controller counted for the drive.")
	drivePredictiveFailuresDesc = driveDesc("predictive_failures_total",
		"Predictive failures the controller counted for the drive.")
	driveBBMErrorsDesc = driveDesc("bbm_errors_total",
		"Bad block management errors the controller counted for the drive.")
	driveLocateDesc = driveDesc("locate_active",
		"1 if the locate LED of the drive is on, 0 otherwise.")
	driveProgressDesc = driveDesc("operation_progress_percent",
		"Progress of a running drive operation in percent.", "operation")
	driveSMARTHealthyDesc = driveDesc("smart_healthy",
		"1 if the drive SMART status reports no failure, 0 otherwise.")
	driveATAValueDesc = driveDesc("ata_smart_attribute_value",
		"Normalized value of an ATA SMART attribute.", "attribute")
	driveATAWorstDesc = driveDesc("ata_smart_attribute_worst",
		"Worst normalized value of an ATA SMART attribute.", "attribute")
	driveATAThresholdDesc = driveDesc("ata_smart_attribute_threshold",
		"Failure threshold of an ATA SMART attribute.", "attribute")
	driveATARawDesc = driveDesc("ata_smart_attribute_raw",
		"Raw value of an ATA SMART attribute.", "attribute")
	driveNVMePercentUsedDesc = driveDesc("nvme_percentage_used",
		"Estimated share of the NVMe drive endurance that is used, in percent.")
	driveNVMeSpareDesc = driveDesc("nvme_available_spare_percent",
		"Remaining spare capacity of the NVMe drive in percent.")
	driveNVMeCriticalWarningDesc = driveDesc("nvme_critical_warning",
		"Raw NVMe critical warning bitmask, 0 means none.")
	driveNVMePowerOnHoursDesc = driveDesc("nvme_power_on_hours_total",
		"Hours the NVMe drive has been powered on.")
	driveNVMePowerCyclesDesc = driveDesc("nvme_power_cycles_total",
		"Power cycles of the NVMe drive.")
	driveNVMeUnsafeShutdownsDesc = driveDesc("nvme_unsafe_shutdowns_total",
		"Unsafe shutdowns of the NVMe drive.")
	driveNVMeMediaErrorsDesc = driveDesc("nvme_media_errors_total",
		"Media and data integrity errors of the NVMe drive.")
	driveNVMeErrorLogDesc = driveDesc("nvme_error_log_entries_total",
		"Error log entries of the NVMe drive.")
)

var driveDescs = []*prometheus.Desc{
	driveMediaErrorsDesc, driveOtherErrorsDesc, drivePredictiveFailuresDesc, driveBBMErrorsDesc,
	driveLocateDesc, driveProgressDesc, driveSMARTHealthyDesc,
	driveATAValueDesc, driveATAWorstDesc, driveATAThresholdDesc, driveATARawDesc,
	driveNVMePercentUsedDesc, driveNVMeSpareDesc, driveNVMeCriticalWarningDesc, driveNVMePowerOnHoursDesc,
	driveNVMePowerCyclesDesc, driveNVMeUnsafeShutdownsDesc, driveNVMeMediaErrorsDesc, driveNVMeErrorLogDesc,
}

func collectDriveDetails(ch chan<- prometheus.Metric, path, ctrl, family string, drives []driveRef, opts DriveOptions) {
	for _, d := range drives {
		labels := []string{ctrl, d.enclosure, d.slot}
		if family == familyMega && (opts.Errors || opts.Locate || opts.Progress) {
			collectMegaDrive(ch, path, d.address, labels, opts)
		}
		if opts.SMART && (family == familyMega || family == familyMPI3 && d.protocol != "SATA") {
			collectDriveSMART(ch, path, d.address, labels)
		}
	}
}

type megaDriveEntry struct {
	Error *string `json:"error"`
	Info  *struct {
		MediaErrors        float64  `json:"media_errors"`
		OtherErrors        float64  `json:"other_errors"`
		PredictiveFailures float64  `json:"predictive_failures"`
		BBMErrorCount      *float64 `json:"bbm_error_count"`
		Progress           struct {
			Rebuild      *progress `json:"rebuild"`
			Patrol       *progress `json:"patrol"`
			Clear        *progress `json:"clear"`
			Erase        *progress `json:"erase"`
			LocateActive bool      `json:"locate_active"`
		} `json:"progress"`
	} `json:"info"`
}

func collectMegaDrive(ch chan<- prometheus.Metric, path, address string, labels []string, opts DriveOptions) {
	ctrl := labels[0]
	var entries []megaDriveEntry
	if err := runJSON(path, &entries, "drive", address, "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("drive "+address+" -c "+ctrl, e.Error) || e.Info == nil {
			continue
		}
		i := e.Info
		if opts.Errors {
			counter(ch, driveMediaErrorsDesc, i.MediaErrors, labels...)
			counter(ch, driveOtherErrorsDesc, i.OtherErrors, labels...)
			counter(ch, drivePredictiveFailuresDesc, i.PredictiveFailures, labels...)
			if i.BBMErrorCount != nil {
				counter(ch, driveBBMErrorsDesc, *i.BBMErrorCount, labels...)
			}
		}
		if opts.Locate {
			gauge(ch, driveLocateDesc, boolValue(i.Progress.LocateActive), labels...)
		}
		if opts.Progress {
			for _, op := range []struct {
				name string
				p    *progress
			}{
				{"clear", i.Progress.Clear},
				{"erase", i.Progress.Erase},
				{"patrol", i.Progress.Patrol},
				{"rebuild", i.Progress.Rebuild},
			} {
				if op.p != nil {
					gauge(ch, driveProgressDesc, op.p.Percent, append(labels, op.name)...)
				}
			}
		}
	}
}

type smartEntry struct {
	Error      *string `json:"error"`
	Healthy    *bool   `json:"healthy"`
	SmartAlert *bool   `json:"smart_alert"`
	IE         *struct {
		FailurePredicted bool `json:"failure_predicted"`
	} `json:"informational_exceptions"`
	ATASmart *struct {
		Health     string `json:"health"`
		Attributes []struct {
			ID        int      `json:"id"`
			Value     float64  `json:"value"`
			Worst     float64  `json:"worst"`
			Threshold *float64 `json:"threshold"`
			Raw       float64  `json:"raw"`
		} `json:"attributes"`
	} `json:"ata_smart"`
	NVMe *struct {
		CriticalWarning float64 `json:"critical_warning"`
		AvailableSpare  float64 `json:"available_spare"`
		PercentageUsed  float64 `json:"percentage_used"`
		PowerCycles     float64 `json:"power_cycles"`
		PowerOnHours    float64 `json:"power_on_hours"`
		UnsafeShutdowns float64 `json:"unsafe_shutdowns"`
		MediaErrors     float64 `json:"media_errors"`
		ErrorLogEntries float64 `json:"error_log_entries"`
	} `json:"nvme"`
}

func (e smartEntry) healthy() bool {
	if e.Healthy != nil {
		return *e.Healthy
	}
	if e.SmartAlert != nil && *e.SmartAlert {
		return false
	}
	if e.IE != nil && e.IE.FailurePredicted {
		return false
	}
	return e.ATASmart == nil || e.ATASmart.Health != "FAILED"
}

func collectDriveSMART(ch chan<- prometheus.Metric, path, address string, labels []string) {
	ctrl := labels[0]
	var entries []smartEntry
	if err := runJSON(path, &entries, "drive", address, "smart", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("drive "+address+" smart -c "+ctrl, e.Error) {
			continue
		}
		gauge(ch, driveSMARTHealthyDesc, boolValue(e.healthy()), labels...)
		if a := e.ATASmart; a != nil {
			for _, attr := range a.Attributes {
				withID := append(append([]string{}, labels...), strconv.Itoa(attr.ID))
				gauge(ch, driveATAValueDesc, attr.Value, withID...)
				gauge(ch, driveATAWorstDesc, attr.Worst, withID...)
				gauge(ch, driveATARawDesc, attr.Raw, withID...)
				if attr.Threshold != nil {
					gauge(ch, driveATAThresholdDesc, *attr.Threshold, withID...)
				}
			}
		}
		if n := e.NVMe; n != nil {
			gauge(ch, driveNVMePercentUsedDesc, n.PercentageUsed, labels...)
			gauge(ch, driveNVMeSpareDesc, n.AvailableSpare, labels...)
			gauge(ch, driveNVMeCriticalWarningDesc, n.CriticalWarning, labels...)
			counter(ch, driveNVMePowerOnHoursDesc, n.PowerOnHours, labels...)
			counter(ch, driveNVMePowerCyclesDesc, n.PowerCycles, labels...)
			counter(ch, driveNVMeUnsafeShutdownsDesc, n.UnsafeShutdowns, labels...)
			counter(ch, driveNVMeMediaErrorsDesc, n.MediaErrors, labels...)
			counter(ch, driveNVMeErrorLogDesc, n.ErrorLogEntries, labels...)
		}
	}
}
