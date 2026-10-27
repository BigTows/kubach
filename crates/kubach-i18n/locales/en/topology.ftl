# Topology screen: the graph of Ingresses, Services, workloads and their
# dependencies, the network policy and traffic layers, and the export.
# Kind names (Ingress, Service, Deployment), "ingress"/"egress", object
# names, label selectors, Service types (headless, NodePort) and the rules
# of NetworkPolicies are data: keep them as they are.

## Filters and toolbar

topology-search-placeholder = Find a node…
topology-labels-placeholder = Labels: app=web,tier!=db
topology-detail-workloads = Workloads
topology-detail-workloads-tooltip = Pods collapsed into their workload
topology-detail-pods = Pods
topology-detail-pods-tooltip = Every pod as a node
# Filter chips that are not kind names.
topology-class-routes = Routes
topology-class-workloads = Workloads
topology-class-external = External
topology-class-hide = Hide
topology-class-show = Show
topology-size =
    { $nodes ->
        [one] { $nodes } node
       *[other] { $nodes } nodes
    } · { $edges ->
        [one] { $edges } edge
       *[other] { $edges } edges
    }
topology-no-matches = No matches
topology-match = Match { $position } of { $count }
topology-policies = Network policies
# $note: topology-policies-scope.
topology-policies-tooltip = Show what NetworkPolicies isolate and block. { $note }
topology-issues = Issues · { $count }
topology-issues-tooltip = Problems found in the graph
topology-broken-references =
    { $count ->
        [one] { $count } broken reference
       *[other] { $count } broken references
    }

## Nothing to show

topology-nothing = Nothing to show
topology-nothing-filtered = No objects match the filters.
topology-nothing-selected-namespaces = No Ingresses, Services or workloads in the selected namespaces.
topology-nothing-any = No Ingresses, Services or workloads.
# $kinds: kinds that can't be listed, like "Ingress, Secret".
topology-error-no-access = No access to { $kinds }: the graph lacks them.
topology-error-cant-refresh = Can't refresh { $kinds }, showing cached data. Retrying…

## Network policy layer

topology-policies-scope = Only standard networking.k8s.io/v1 NetworkPolicies are analyzed (no Cilium, Calico or other CRDs); whether they are enforced depends on the CNI.
topology-policies-loading = Network policies: loading…
topology-policies-none = Network policies: none in the selected namespaces.
topology-policies-count =
    { $count ->
        [one] Network policies: { $count } policy.
       *[other] Network policies: { $count } policies.
    }
# $lead: one of the three above; $note: topology-policies-scope.
topology-policies-note = { $lead } Isolated workloads carry a shield; red dashed edges are Service traffic blocked by policy, green dotted ones what a policy explicitly allows. { $note }
topology-policy-section = Network policy
# $direction: "ingress" or "egress".
topology-not-isolated = Not isolated: all { $direction } allowed
# $policies: "namespace/name" of the policies.
topology-isolated-by = Isolated by { $policies }
topology-policy-deny-all = { $direction }: deny all
topology-reach = Can reach…
topology-reach-target = Target
topology-reach-target-label = Target to reach
topology-reach-search = Find a workload or Service…
topology-port-placeholder = Port: 8080, http, 53/udp
topology-port-empty = Enter a port
topology-port-invalid = Not a port: { $text }
topology-port-out-of-range = Port out of range: { $port }
topology-reach-no-pods = The Service selects no pods: nothing to reach.
topology-reach-allowed = Allowed
topology-reach-blocked = Blocked
# $port: "TCP 8080"; $via: the pods checked, like "Deployment shop/web".
topology-reach-via = { $port } · via { $via }
topology-reach-egress = Egress of this workload
topology-reach-ingress = Ingress of the target

## Graph

topology-presence-missing = missing
topology-presence-unknown = unknown
topology-isolated = isolated
topology-isolated-in = isolated in
topology-isolated-out = isolated out
# Text in the middle of an edge.
topology-edge-allowed = allowed
topology-edge-allows = allows { $ports }
topology-edge-blocked = blocked by policy
topology-edge-ready = { $count } ready
topology-edge-ready-of = { $count }/{ $total } ready
# Measured traffic. $rate: "12", "1.2k"; $percent: "2.1", "<0.1".
topology-traffic-requests = { $rate } req/s
topology-traffic-flows = { $rate } flows/s
topology-traffic-errors = { $percent }% err
topology-traffic-dropped = { $percent }% dropped
topology-traffic-idle = idle
# An edge kind in the details: "Service web · routes to".
topology-edge-routes-to = routes to
topology-edge-selects = selects
topology-edge-owns = owns
topology-edge-mounts = mounts
topology-edge-env-from = env from
topology-edge-service-account = service account
topology-edge-exposes = exposes
topology-edge-tls = TLS secret
topology-edge-policy-allows = policy allows
topology-edge-traffic = traffic
topology-edge-links-to = links to
# Problems. $node and $target: "Service shop/web".
topology-diagnostic-no-ready = { $node } has no ready endpoints
topology-diagnostic-not-ready = { $node } has no ready endpoints ({ $count } not ready)
topology-diagnostic-selector = { $node } selector matches no pods
topology-diagnostic-missing-target = { $node } refers to missing { $target }
topology-diagnostic-missing-port = { $node } refers to port { $port } that { $target } does not have

## Issues and details

topology-no-problems = No problems found.
topology-open-details = Open details
topology-problems = Problems
topology-overview = Overview
topology-fact-pods-ready = Pods ready
topology-of = { $count } of { $total }
topology-fact-replicas = Replicas
topology-fact-phase = Phase
topology-fact-cluster-ip = Cluster IP
topology-fact-external-name = External name
topology-fact-ports = Ports
topology-fact-hosts = Hosts
topology-fact-class = Class
topology-show-on-graph = Show on the graph
topology-incoming = Incoming
topology-outgoing = Outgoing
topology-missing = Missing
topology-not-loaded = Not loaded

## Traffic layer

traffic-button = Traffic
traffic-button-tooltip = Show measured traffic from the Prometheus metrics of Istio, Linkerd or Cilium Hubble
traffic-detect-again-tooltip = Look for traffic metrics in Prometheus again
# $reason: why there is no Prometheus (the traffic-prometheus-* below or a
# Prometheus problem).
traffic-needs-prometheus = Traffic needs Prometheus with mesh or CNI metrics: { $reason }.
traffic-prometheus-off = Prometheus is off for this context in Settings
traffic-prometheus-looking = looking for Prometheus…
traffic-prometheus-not-found = no Prometheus found in the cluster
traffic-prometheus-no-address = no Prometheus address set in Settings
traffic-prometheus-unavailable = Prometheus is not available
traffic-detecting = Traffic: looking for Istio, Linkerd or Cilium Hubble metrics…
traffic-no-sources = No traffic metrics sources: Istio, Linkerd and Cilium Hubble are not installed in the cluster. The graph shows declared connections only.
# $sources: "Istio, Linkerd"; $count: how many.
traffic-no-metrics =
    { $count ->
        [one] No traffic metrics sources: { $sources } is installed, but Prometheus has no traffic metrics from it. The graph shows declared connections only.
       *[other] No traffic metrics sources: { $sources } are installed, but Prometheus has no traffic metrics from them. The graph shows declared connections only.
    }
# $window: "5m".
traffic-window-tooltip = Rates over the last { $window }
# $source: "Istio"; $count: flows measured.
traffic-lead-loading = Traffic: { $source } · loading…
traffic-lead-none = Traffic: { $source } · no traffic in the window
traffic-lead =
    { $count ->
        [one] Traffic: { $source } · { $count } flow
       *[other] Traffic: { $source } · { $count } flows
    }
traffic-lead-busiest =
    { $count ->
        [one] Traffic: { $source } · { $count } flow (busiest shown)
       *[other] Traffic: { $source } · { $count } flows (busiest shown)
    }
traffic-also-found = Also found: { $sources } (not used).
traffic-poll-failed = Last poll failed: { $error }

## Export

topology-export = Export
topology-export-tooltip = Export the graph as shown
topology-export-mermaid = Copy Mermaid
topology-export-dot = Copy Graphviz DOT
topology-export-svg = Save SVG…
topology-export-png = Save PNG…
# $format: "Mermaid" or "Graphviz DOT".
topology-export-copied = Topology copied as { $format }
topology-export-saved = Saved { $path }
topology-export-save-failed = Could not save { $path }: { $error }
