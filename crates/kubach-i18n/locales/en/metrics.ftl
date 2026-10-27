# Metrics: the Metrics block of the details, the usage columns of the Pods
# and Nodes tables, the Prometheus of a context, and the Overview screen.
# metrics-server, Prometheus, kube-state-metrics, Pods, Nodes, pod phases
# (Running, Pending), ranges ("15m"), quantities ("250m", "512Mi") and
# Prometheus addresses are data: keep them as they are.

## Metrics block

metrics-memory = Memory
metrics-network = Network
metrics-filesystem = Filesystem
# Lines of a chart.
metrics-line-working-set = Working set
metrics-line-receive = Receive
metrics-line-transmit = Transmit
metrics-line-read = Read
metrics-line-write = Write
metrics-total = Total
metrics-by-container = By container
metrics-by-pod = By pod
# Right end of the time axis.
metrics-now = now
# Dashed lines of a chart.
metrics-request = Request
metrics-limit = Limit
metrics-allocatable = Allocatable
metrics-looking-for-prometheus = Looking for Prometheus…
metrics-no-source = No metrics: no Prometheus or metrics-server found
metrics-needs-prometheus = Needs Prometheus: metrics-server reports only CPU and memory
metrics-waiting-server = Waiting for metrics-server…
metrics-server-no-answer = metrics-server did not answer
metrics-server-no-sample = metrics-server has no sample for this object yet
metrics-loading-prometheus = Loading from Prometheus…
metrics-prometheus-no-answer = Prometheus did not answer
metrics-no-data = No data in Prometheus for this range
metrics-server-while-open = metrics-server · while the panel is open
metrics-unavailable = metrics unavailable
# Why a range is disabled (kubach-core). $range: "15m"; $age: how much data
# there is, like "40m"; $subject: object (one object) or pods (the pods of
# the Overview screen).
metrics-coverage-less =
    { $subject ->
        [pods] Prometheus has less than { $range } of data for these pods
       *[object] Prometheus has less than { $range } of data for this object
    }
metrics-coverage-same =
    { $subject ->
        [pods] Prometheus has only { $age } of data for these pods: same as { $range }
       *[object] Prometheus has only { $age } of data for this object: same as { $range }
    }
# Without Prometheus (kubach-core). $place: panel (details) or screen
# (Overview).
metrics-live-collected =
    { $place ->
        [screen] Collected since the screen opened: { $age } so far
       *[panel] Collected since the panel opened: { $age } so far
    }

## Prometheus of a context

# $endpoint: "monitoring/prometheus:9090".
prometheus-doesnt-answer = Prometheus at { $endpoint } doesn't answer
prometheus-invalid-address = Prometheus address { $address } is not valid
prometheus-unusual-namespace = Prometheus { $endpoint } is in an unusual namespace: not used until confirmed
prometheus-use-it = Use it
prometheus-use-it-tooltip = Use this Prometheus for the context
prometheus-detect-again = Detect again
prometheus-settings = Settings
# Why the search could not run. $error: the connection error (English).
prometheus-cannot-connect = cannot connect: { $error }

## Overview screen

overview-all-namespaces = all namespaces
overview-namespaces = { $count } namespaces
# Why Nodes or Pods have no count.
overview-not-allowed = not allowed to list
overview-cant-list = can't be listed
overview-ready = ready
overview-not-ready = not ready
overview-whole-cluster = whole cluster
overview-restarts =
    { $count ->
        [one] { $count } restart
       *[other] { $count } restarts
    }
# $recent: restarts in the last $range ("1h").
overview-restarts-recent =
    { $count ->
        [one] { $count } restart · { $recent } in the last { $range }
       *[other] { $count } restarts · { $recent } in the last { $range }
    }
overview-restarts-since =
    { $count ->
        [one] { $count } restart · { $since } since the screen opened
       *[other] { $count } restarts · { $since } since the screen opened
    }
# Under the share of pod slots taken.
overview-of-slots = of slots
overview-pods-of = { $count } / { $capacity } pods
# $scope: overview-all-namespaces, a namespace or overview-namespaces.
overview-in-scope = in { $scope }
overview-pod-slots-note = Pod slots: allocatable pods of all nodes (whole cluster)
# Under the share in a CPU or memory ring.
overview-used = used
overview-requested = requested
overview-unlimited =
    { $count ->
        [one] { $count } pod without a limit
       *[other] { $count } pods without a limit
    }
overview-limits = Limits
overview-segment-used = Used
overview-requests = Requests
overview-used-of-allocatable = used of allocatable
overview-usage-unknown = usage unknown
overview-pods-scope = pods: { $scope }
# $source: metrics-server or Prometheus.
overview-usage-from = Usage from { $source } · max: allocatable, whole cluster
overview-max-allocatable = Max: allocatable, whole cluster
overview-cores = { $value } / { $max } cores
overview-usage-server-no-answer = Usage unavailable: metrics-server did not answer
overview-usage-loading-prometheus = Loading usage from Prometheus…
overview-usage-prometheus-no-answer = Usage unavailable: Prometheus did not answer
overview-usage-no-samples = Usage unavailable: Prometheus has no samples for pods in { $scope }
overview-usage-see-problem = Usage unavailable: see the Prometheus problem above
overview-usage-none = Usage unavailable: install metrics-server or set Prometheus in Settings → Clusters
overview-cpu-memory = CPU and memory
overview-history = Pods in { $scope }
overview-history-prometheus = Pods in { $scope } · Prometheus · { $endpoint }
overview-history-session = Pods in { $scope } · metrics-server · while the screen is open
overview-pods-by-phase = Pods by phase
overview-phases-prometheus = { $scope } · kube-state-metrics in Prometheus
overview-phases-session = { $scope } · from the cache, while the screen is open
overview-phases-collecting = Collecting pod counts: the chart grows while the screen is open
overview-metrics-unavailable = Metrics unavailable
overview-metrics-unavailable-description = No metrics-server or Prometheus for this context: usage and charts need one of them. Counters from the cluster still work.
overview-prometheus-settings = Prometheus settings
overview-cluster = Cluster
overview-context = Context
overview-user = User
overview-server-version = Server version
overview-api-resources-title = API resources
overview-discovering = Discovering…
overview-api-resources = { $kinds } kinds in { $groups } groups
overview-custom-resources = Custom resources
