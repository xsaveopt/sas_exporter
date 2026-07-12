package main

import (
	"flag"
	"log"
	"net/http"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/xsaveopt/sas_exporter/collector"
)

var version = "0.1.0"

func main() {
	var (
		listenAddr   = flag.String("web.listen-address", ":9856", "Address on which to expose metrics.")
		metricsPath  = flag.String("web.telemetry-path", "/metrics", "Path under which to expose metrics.")
		sas3ircuPath = flag.String("sas3ircu", "sas3ircu", "Path to the sas3ircu binary.")
		sas2ircuPath = flag.String("sas2ircu", "sas2ircu", "Path to the sas2ircu binary.")
		storCLIPath  = flag.String("storcli", "storcli", "Path to the storcli binary.")
		hwmonRoot    = flag.String("hwmon.path", "/sys/class/hwmon", "Path to the hwmon sysfs root.")
	)
	flag.Parse()

	reg := prometheus.NewRegistry()
	reg.MustRegister(
		collectors.NewGoCollector(),
		collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}),
		collector.NewIrcuCollector(*sas3ircuPath, *sas2ircuPath),
		collector.NewStorCLICollector(*storCLIPath),
		collector.NewHwmonCollector(*hwmonRoot),
	)

	http.Handle(*metricsPath, promhttp.HandlerFor(reg, promhttp.HandlerOpts{}))
	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`<!DOCTYPE html>
<html><head><title>SAS Exporter</title></head>
<body>
<h1>SAS HBA Exporter</h1>
<p><a href="` + *metricsPath + `">Metrics</a></p>
<p>Version: ` + version + `</p>
</body></html>`))
	})

	log.Printf("sas_exporter %s listening on %s", version, *listenAddr)
	log.Fatal(http.ListenAndServe(*listenAddr, nil))
}
