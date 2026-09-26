package collector

import (
	"log"
	"maps"
	"slices"
	"strconv"
	"strings"

	"github.com/prometheus/client_golang/prometheus"
)

func newDesc(subsystem, name, help string, labels ...string) *prometheus.Desc {
	return prometheus.NewDesc(prometheus.BuildFQName(namespace, subsystem, name), help, labels, nil)
}

var (
	iocExceptionsDesc = newDesc("controller", "ioc_exceptions",
		"Raw IOC exceptions bitmask reported by the controller, 0 means none.", "controller")
	controllerDevicesDesc = newDesc("controller", "devices",
		"Devices the controller sees, by type.", "controller", "type")
	volumesDegradedDesc = newDesc("controller", "volumes_degraded",
		"Volumes the controller reports as degraded.", "controller")
	volumesOfflineDesc = newDesc("controller", "volumes_offline",
		"Volumes the controller reports as offline.", "controller")
	drivesFailedDesc = newDesc("controller", "drives_failed",
		"Drives the controller reports as failed.", "controller")
	drivesPredictiveFailureDesc = newDesc("controller", "drives_predictive_failure",
		"Drives the controller reports with a predictive failure.", "controller")
	memoryErrorsDesc = newDesc("controller", "memory_errors_total",
		"Controller memory errors, by type.", "controller", "type")
	batteryPresentDesc = newDesc("controller", "battery_present",
		"1 if the controller has a BBU or CacheVault, 0 otherwise.", "controller")
	alarmPresentDesc = newDesc("controller", "alarm_present",
		"1 if the controller has an alarm, 0 otherwise.", "controller")

	phyEnabledDesc = newDesc("phy", "enabled",
		"1 if the controller phy is enabled, 0 otherwise.", "controller", "phy")
	phyLinkRateDesc = newDesc("phy", "link_rate_gbps",
		"Negotiated link rate of the controller phy in Gb/s, 0 when there is no link.", "controller", "phy")
	phyMaxLinkRateDesc = newDesc("phy", "max_link_rate_gbps",
		"Highest link rate the controller phy supports in Gb/s.", "controller", "phy")
	phyErrorsDesc = newDesc("phy", "errors_total",
		"Controller phy link error counters, by type.", "controller", "phy", "type")

	volumeInfoDesc = newDesc("volume", "info",
		"RAID volume information, always 1.", "controller", "volume", "name", "raid_level", "state")
	volumeProgressDesc = newDesc("volume", "operation_progress_percent",
		"Progress of a running volume operation in percent.", "controller", "volume", "operation")

	batteryChargeDesc = newDesc("battery", "charge_percent",
		"Relative charge of the battery in percent.", "controller")
	batteryRemainingDesc = newDesc("battery", "remaining_capacity_mah",
		"Remaining battery capacity in mAh.", "controller")
	batteryFullDesc = newDesc("battery", "full_charge_capacity_mah",
		"Battery capacity when fully charged in mAh.", "controller")
	batteryDesignDesc = newDesc("battery", "design_capacity_mah",
		"Design capacity of the battery in mAh.", "controller")
	batteryCyclesDesc = newDesc("battery", "cycles_total",
		"Charge cycles the battery has been through.", "controller")
	batteryVoltageDesc = newDesc("battery", "voltage_volts",
		"Battery voltage in volts.", "controller")
	batteryCurrentDesc = newDesc("battery", "current_amps",
		"Battery current in amps, negative while discharging.", "controller")
	batteryTempDesc = newDesc("battery", "temperature_celsius",
		"Battery temperature in Celsius.", "controller")
	batteryHealthDesc = newDesc("battery", "health_good",
		"1 if the battery reports a good state of health, 0 otherwise.", "controller")

	patrolInfoDesc = newDesc("patrol", "info",
		"Patrol read state and mode, always 1.", "controller", "state", "mode")
	patrolIterationsDesc = newDesc("patrol", "iterations_total",
		"Completed patrol read iterations.", "controller")
	patrolDrivesDoneDesc = newDesc("patrol", "drives_done",
		"Drives the running patrol read has finished.", "controller")
	patrolNextRunDesc = newDesc("patrol", "next_run_seconds",
		"Seconds until the next scheduled patrol read.", "controller")
)

var controllerDescs = []*prometheus.Desc{
	iocExceptionsDesc, controllerDevicesDesc, volumesDegradedDesc, volumesOfflineDesc,
	drivesFailedDesc, drivesPredictiveFailureDesc, memoryErrorsDesc, batteryPresentDesc, alarmPresentDesc,
	phyEnabledDesc, phyLinkRateDesc, phyMaxLinkRateDesc, phyErrorsDesc,
	volumeInfoDesc, volumeProgressDesc,
	batteryChargeDesc, batteryRemainingDesc, batteryFullDesc, batteryDesignDesc, batteryCyclesDesc,
	batteryVoltageDesc, batteryCurrentDesc, batteryTempDesc, batteryHealthDesc,
	patrolInfoDesc, patrolIterationsDesc, patrolDrivesDoneDesc, patrolNextRunDesc,
}

func gauge(ch chan<- prometheus.Metric, desc *prometheus.Desc, value float64, labels ...string) {
	ch <- prometheus.MustNewConstMetric(desc, prometheus.GaugeValue, value, labels...)
}

func counter(ch chan<- prometheus.Metric, desc *prometheus.Desc, value float64, labels ...string) {
	ch <- prometheus.MustNewConstMetric(desc, prometheus.CounterValue, value, labels...)
}

func boolValue(b bool) float64 {
	if b {
		return 1
	}
	return 0
}

func entryFailed(cmd string, err *string) bool {
	if err == nil {
		return false
	}
	log.Printf("sas_exporter: sasctl %s: %s", cmd, *err)
	return true
}

type controllerDetail struct {
	Error         *string            `json:"error"`
	IOCExceptions *float64           `json:"ioc_exceptions"`
	Devices       map[string]float64 `json:"devices"`
	Info          *struct {
		LDDegraded             float64 `json:"ld_degraded"`
		LDOffline              float64 `json:"ld_offline"`
		PDDiskFailed           float64 `json:"pd_disk_failed"`
		PDDiskPredFailure      float64 `json:"pd_disk_pred_failure"`
		MemCorrectableErrors   float64 `json:"mem_correctable_errors"`
		MemUncorrectableErrors float64 `json:"mem_uncorrectable_errors"`
		BBUPresent             bool    `json:"bbu_present"`
		AlarmPresent           bool    `json:"alarm_present"`
	} `json:"info"`
}

func collectControllerDetail(ch chan<- prometheus.Metric, path, ctrl string) {
	var entries []controllerDetail
	if err := runJSON(path, &entries, "controller", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("controller -c "+ctrl, e.Error) {
			continue
		}
		if e.IOCExceptions != nil {
			gauge(ch, iocExceptionsDesc, *e.IOCExceptions, ctrl)
		}
		for _, kind := range slices.Sorted(maps.Keys(e.Devices)) {
			gauge(ch, controllerDevicesDesc, e.Devices[kind], ctrl, kind)
		}
		if i := e.Info; i != nil {
			gauge(ch, volumesDegradedDesc, i.LDDegraded, ctrl)
			gauge(ch, volumesOfflineDesc, i.LDOffline, ctrl)
			gauge(ch, drivesFailedDesc, i.PDDiskFailed, ctrl)
			gauge(ch, drivesPredictiveFailureDesc, i.PDDiskPredFailure, ctrl)
			counter(ch, memoryErrorsDesc, i.MemCorrectableErrors, ctrl, "correctable")
			counter(ch, memoryErrorsDesc, i.MemUncorrectableErrors, ctrl, "uncorrectable")
			gauge(ch, batteryPresentDesc, boolValue(i.BBUPresent), ctrl)
			gauge(ch, alarmPresentDesc, boolValue(i.AlarmPresent), ctrl)
		}
	}
}

func linkRateGbps(rate *string) (float64, bool) {
	if rate == nil {
		return 0, false
	}
	value, ok := strings.CutSuffix(*rate, " Gb/s")
	if !ok {
		return 0, true
	}
	f, err := strconv.ParseFloat(value, 64)
	if err != nil {
		return 0, true
	}
	return f, true
}

type phyEntry struct {
	Error *string `json:"error"`
	Phys  []struct {
		Phy       int     `json:"phy"`
		Enabled   bool    `json:"enabled"`
		LinkRate  *string `json:"link_rate"`
		HWMaxRate *string `json:"hw_max_rate"`
		Counters  *struct {
			InvalidDword         float64 `json:"invalid_dword_count"`
			RunningDisparity     float64 `json:"running_disparity_error_count"`
			LossDwordSynch       float64 `json:"loss_dword_synch_count"`
			PhyResetProblemCount float64 `json:"phy_reset_problem_count"`
		} `json:"counters"`
	} `json:"phys"`
}

func collectPhys(ch chan<- prometheus.Metric, path, ctrl string) {
	var entries []phyEntry
	if err := runJSON(path, &entries, "phy", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
	}
	for _, e := range entries {
		if entryFailed("phy -c "+ctrl, e.Error) {
			continue
		}
		for _, p := range e.Phys {
			phy := strconv.Itoa(p.Phy)
			gauge(ch, phyEnabledDesc, boolValue(p.Enabled), ctrl, phy)
			if rate, ok := linkRateGbps(p.LinkRate); ok {
				gauge(ch, phyLinkRateDesc, rate, ctrl, phy)
			}
			if rate, ok := linkRateGbps(p.HWMaxRate); ok {
				gauge(ch, phyMaxLinkRateDesc, rate, ctrl, phy)
			}
		}
	}

	entries = nil
	if err := runJSON(path, &entries, "phy", "errors", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("phy errors -c "+ctrl, e.Error) {
			continue
		}
		for _, p := range e.Phys {
			if p.Counters == nil {
				continue
			}
			phy := strconv.Itoa(p.Phy)
			counter(ch, phyErrorsDesc, p.Counters.InvalidDword, ctrl, phy, "invalid_dword")
			counter(ch, phyErrorsDesc, p.Counters.RunningDisparity, ctrl, phy, "running_disparity")
			counter(ch, phyErrorsDesc, p.Counters.LossDwordSynch, ctrl, phy, "loss_dword_sync")
			counter(ch, phyErrorsDesc, p.Counters.PhyResetProblemCount, ctrl, phy, "phy_reset_problem")
		}
	}
}

type progress struct {
	Percent float64 `json:"percent"`
}

type volumeEntry struct {
	Error   *string `json:"error"`
	Volumes []struct {
		ID        *int    `json:"id"`
		TargetID  *int    `json:"target_id"`
		Name      *string `json:"name"`
		RAIDLevel *string `json:"raid_level"`
		RAID      *string `json:"raid"`
		State     string  `json:"state"`
		Status    *struct {
			CurrentOperation string `json:"current_operation"`
			Progress         *struct {
				PercentComplete float64 `json:"percent_complete"`
			} `json:"progress"`
		} `json:"status"`
	} `json:"volumes"`
}

type volumeProgressEntry struct {
	Error      *string              `json:"error"`
	Operations map[string]*progress `json:"operations"`
}

func collectVolumes(ch chan<- prometheus.Metric, path, ctrl, family string) {
	var entries []volumeEntry
	if err := runJSON(path, &entries, "volume", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("volume -c "+ctrl, e.Error) {
			continue
		}
		for _, v := range e.Volumes {
			idPtr := v.ID
			if idPtr == nil {
				idPtr = v.TargetID
			}
			if idPtr == nil {
				continue
			}
			id := strconv.Itoa(*idPtr)
			level := v.RAIDLevel
			if level == nil {
				level = v.RAID
			}
			gauge(ch, volumeInfoDesc, 1, ctrl, id, deref(v.Name), deref(level), stateCode(v.State))
			if s := v.Status; s != nil && s.Progress != nil {
				gauge(ch, volumeProgressDesc, s.Progress.PercentComplete, ctrl, id, operationName(s.CurrentOperation))
			}
			if family == familyMega {
				collectMegaVolumeProgress(ch, path, ctrl, id)
			}
		}
	}
}

func operationName(op string) string {
	return strings.ReplaceAll(strings.ToLower(op), " ", "_")
}

func collectMegaVolumeProgress(ch chan<- prometheus.Metric, path, ctrl, id string) {
	var entries []volumeProgressEntry
	if err := runJSON(path, &entries, "volume", id, "progress", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("volume "+id+" progress -c "+ctrl, e.Error) {
			continue
		}
		for _, op := range slices.Sorted(maps.Keys(e.Operations)) {
			if p := e.Operations[op]; p != nil {
				gauge(ch, volumeProgressDesc, p.Percent, ctrl, id, op)
			}
		}
	}
}

type batteryEntry struct {
	Status *struct {
		VoltageMV          float64 `json:"voltage_mv"`
		CurrentMA          float64 `json:"current_ma"`
		TemperatureCelsius float64 `json:"temperature_celsius"`
		BBU                *struct {
			RelativeChargePercent float64 `json:"relative_charge_percent"`
			StateOfHealthGood     bool    `json:"state_of_health_good"`
		} `json:"bbu"`
	} `json:"status"`
	Capacity *struct {
		RelativeChargePercent float64 `json:"relative_charge_percent"`
		RemainingCapacityMAh  float64 `json:"remaining_capacity_mah"`
		FullChargeCapacityMAh float64 `json:"full_charge_capacity_mah"`
		CycleCount            float64 `json:"cycle_count"`
	} `json:"capacity"`
	Design *struct {
		DesignCapacityMAh float64 `json:"design_capacity_mah"`
	} `json:"design"`
}

func collectBattery(ch chan<- prometheus.Metric, path, ctrl string) {
	var entries []batteryEntry
	if err := runJSON(path, &entries, "battery", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if s := e.Status; s != nil {
			gauge(ch, batteryVoltageDesc, s.VoltageMV/1000, ctrl)
			gauge(ch, batteryCurrentDesc, s.CurrentMA/1000, ctrl)
			gauge(ch, batteryTempDesc, s.TemperatureCelsius, ctrl)
			if s.BBU != nil {
				gauge(ch, batteryHealthDesc, boolValue(s.BBU.StateOfHealthGood), ctrl)
				if e.Capacity == nil {
					gauge(ch, batteryChargeDesc, s.BBU.RelativeChargePercent, ctrl)
				}
			}
		}
		if c := e.Capacity; c != nil {
			gauge(ch, batteryChargeDesc, c.RelativeChargePercent, ctrl)
			gauge(ch, batteryRemainingDesc, c.RemainingCapacityMAh, ctrl)
			gauge(ch, batteryFullDesc, c.FullChargeCapacityMAh, ctrl)
			counter(ch, batteryCyclesDesc, c.CycleCount, ctrl)
		}
		if d := e.Design; d != nil {
			gauge(ch, batteryDesignDesc, d.DesignCapacityMAh, ctrl)
		}
	}
}

type patrolEntry struct {
	Error  *string `json:"error"`
	Status *struct {
		Iterations float64 `json:"iterations"`
		StateName  string  `json:"state_name"`
		DrivesDone float64 `json:"drives_done"`
	} `json:"status"`
	Properties *struct {
		ModeName string `json:"mode_name"`
	} `json:"properties"`
	NextRunInSeconds *float64 `json:"next_run_in_seconds"`
}

func collectPatrol(ch chan<- prometheus.Metric, path, ctrl string) {
	var entries []patrolEntry
	if err := runJSON(path, &entries, "patrol", "-c", ctrl); err != nil {
		log.Printf("sas_exporter: %v", err)
		return
	}
	for _, e := range entries {
		if entryFailed("patrol -c "+ctrl, e.Error) || e.Status == nil {
			continue
		}
		mode := ""
		if e.Properties != nil {
			mode = e.Properties.ModeName
		}
		gauge(ch, patrolInfoDesc, 1, ctrl, e.Status.StateName, mode)
		counter(ch, patrolIterationsDesc, e.Status.Iterations, ctrl)
		gauge(ch, patrolDrivesDoneDesc, e.Status.DrivesDone, ctrl)
		if e.NextRunInSeconds != nil {
			gauge(ch, patrolNextRunDesc, *e.NextRunInSeconds, ctrl)
		}
	}
}
