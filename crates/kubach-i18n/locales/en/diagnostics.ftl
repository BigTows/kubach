# Pod diagnostics and security findings in the details panel, and phrases
# of the built-in plugins' detail fields. kubach-core writes them as
# messages (English for the MCP agent); the app translates them. Names,
# images, reasons, exit codes and messages of the cluster are data.


## Notes of the Diagnostics and Security sections

# $severity: diagnostic-severity-* or security-severity-*; $title: a title.
details-note-title = { $severity }: { $title }
details-note-hint = Hint: { $hint }
diagnostic-severity-error = Error
diagnostic-severity-warning = Warning
diagnostic-severity-info = Info


## Pod diagnostics (kubach-core/src/diagnostics.rs)
## $container is diagnostic-container or diagnostic-init-container,
## $restarts is diagnostic-restarts, $exit is diagnostic-exit-*,
## $duration is short like "12m".

diagnostic-container = Container `{ $name }`
diagnostic-init-container = Init container `{ $name }`
diagnostic-restarts =
    { $count ->
        [one] { $count } restart
       *[other] { $count } restarts
    }
diagnostic-exit-code = exit code { $code }
diagnostic-exit-reason = { $reason }, exit code { $code }
diagnostic-last-run = Last run ended with exit code { $code }.
diagnostic-last-run-signal = Last run ended with exit code { $code } ({ $signal }).
diagnostic-last-run-reason = Last run ended with exit code { $code }, reason { $reason }.
diagnostic-last-run-signal-reason = Last run ended with exit code { $code } ({ $signal }), reason { $reason }.

diagnostic-terminating = Pod is terminating
diagnostic-terminating-detail = Deletion requested; grace period ends in { $duration }.
diagnostic-stuck = Pod is stuck terminating for { $duration }
diagnostic-stuck-detail = Grace period ended { $duration } ago but the pod still exists.
diagnostic-stuck-finalizers-detail = Grace period ended { $duration } ago; finalizers still pending: { $finalizers }.
diagnostic-stuck-hint = Check that the node is reachable and the kubelet is healthy; force delete as a last resort
diagnostic-stuck-finalizers-hint = Check the controllers owning the finalizers, or remove them if those controllers are gone

diagnostic-evicted = Pod was evicted
diagnostic-evicted-hint = The node ran low on resources; set requests to match actual usage
diagnostic-pod-failed = Pod failed: { $reason }

diagnostic-gated = Pod is waiting for scheduling gates to be removed
diagnostic-gated-hint = Check spec.schedulingGates and the controller that owns them
diagnostic-unschedulable = Pod cannot be scheduled
# $causes: diagnostic-cause-* joined with commas.
diagnostic-unschedulable-causes = Pod cannot be scheduled: { $causes }
diagnostic-unschedulable-hint = Read the scheduler message and compare the pod spec with available nodes
diagnostic-cause-cpu = insufficient CPU
diagnostic-cause-memory = insufficient memory
# $resource: a resource name like nvidia.com/gpu.
diagnostic-cause-resource = insufficient { $resource }
diagnostic-cause-taints = untolerated taints
diagnostic-cause-selector = no node matches selector/affinity
diagnostic-cause-pod-affinity = pod affinity rules not met
diagnostic-cause-pvc = unbound PersistentVolumeClaim
diagnostic-cause-volume-affinity = volume node affinity conflict
diagnostic-cause-ports = host ports in use
diagnostic-resources-hint = Lower the container requests or add capacity to the cluster
diagnostic-taints-hint = Add matching tolerations or schedule onto nodes without these taints
diagnostic-selector-hint = Check nodeSelector/affinity against node labels
diagnostic-pod-affinity-hint = Check podAffinity/podAntiAffinity rules against running pods
diagnostic-pvc-hint = Check that the PVC exists and its StorageClass can provision a volume
diagnostic-volume-affinity-hint = The bound volume lives in a zone/node the pod cannot be scheduled to
diagnostic-ports-hint = Drop hostPort or free the port on the nodes

diagnostic-crash-looping = { $container } is crash looping ({ $restarts })
diagnostic-crash-looping-exit = { $container } is crash looping ({ $exit }, { $restarts })
diagnostic-previous-logs-hint = Check logs of the previous run
diagnostic-image-pull = { $container } cannot pull image `{ $image }` ({ $reason })
diagnostic-image-name-hint = Fix the image reference in the pod spec
diagnostic-never-pull-hint = imagePullPolicy is Never but the image is not on the node
diagnostic-credentials-hint = Check imagePullSecrets and registry credentials
diagnostic-image-missing-hint = Check that the image name and tag exist in the registry
diagnostic-image-hint = Check the image name, tag and that the node can reach the registry
diagnostic-missing-configmap = { $container } references missing ConfigMap `{ $name }`
diagnostic-missing-secret = { $container } references missing Secret `{ $name }`
diagnostic-invalid-config = { $container } has an invalid configuration
diagnostic-config-hint = Create the referenced ConfigMap/Secret or fix env/envFrom references
diagnostic-waiting = { $container } is waiting: { $reason }
diagnostic-failed = { $container } failed ({ $exit })
diagnostic-logs-hint = Check the container logs
diagnostic-restarted = { $container } restarted ({ $exit }, { $restarts })

diagnostic-oom = { $container } was OOMKilled ({ $restarts })
diagnostic-oom-looping = { $container } was OOMKilled ({ $restarts }, crash looping)
diagnostic-oom-limit = { $container } was OOMKilled (limit { $limit }, { $restarts })
diagnostic-oom-limit-looping = { $container } was OOMKilled (limit { $limit }, { $restarts }, crash looping)
diagnostic-oom-limit-detail = The container exceeded its memory limit of { $limit } and was killed.
diagnostic-oom-limit-hint = Raise the memory limit above { $limit } or reduce memory usage
diagnostic-oom-detail = The container was killed for using too much memory; it has no memory limit, so the node itself ran out of memory.
diagnostic-oom-hint = Set memory requests/limits that match the container's actual usage

diagnostic-startup-probe = { $container } has not passed its startup probe
diagnostic-startup-probe-hint = Check the startup probe and the container logs
diagnostic-not-ready = { $container } is running but not ready
diagnostic-not-ready-hint = Check the readiness probe and the container logs
diagnostic-pod-not-ready = Pod is running but not ready
diagnostic-pod-not-ready-hint = Check spec.readinessGates and the pod conditions

diagnostic-volume-mount = A volume cannot be mounted
diagnostic-volume-configmap-hint = Create ConfigMap `{ $name }` or fix the volume reference
diagnostic-volume-secret-hint = Create Secret `{ $name }` or fix the volume reference
diagnostic-volume-hint = Check the volume, its PVC and the storage driver
diagnostic-sandbox = Pod sandbox cannot be created
diagnostic-sandbox-hint = Check the CNI plugin and container runtime on the node


## Phrases of detail fields with data in them

# Two parts of a value, like "HTTPS :443 · TLS Terminate".
details-joined = { $first } · { $second }
gateway-listener-port = { $protocol } :{ $port }
gateway-listener-host = host { $host }
# $mode: Terminate or Passthrough.
gateway-listener-tls = TLS { $mode }
# $from: All, Same or Selector.
gateway-listener-routes-from = routes from { $from }
gateway-listener-routes-attached = { $count } routes attached
# $plural: the plural name of a CRD, like "certificates".
crd-show-objects = Show { $plural }
# A CRD version with its notes: "v1alpha1 (not served, deprecated)".
crd-version-notes = { $version } ({ $notes })
crd-version-storage = storage
crd-version-not-served = not served
crd-version-deprecated = deprecated
# $message: the condition's message from the cluster; $hint: what to do.
namespace-condition-hint = { $message } — { $hint }
namespace-finalizers-hint = the controllers that own these finalizers must remove them (or they are removed by hand)
namespace-discovery-hint = an aggregated API (often metrics.k8s.io) does not answer; fix or remove its APIService
