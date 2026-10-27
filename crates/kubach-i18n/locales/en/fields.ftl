# Texts of the built-in resource plugins of kubach-core in the details panel:
# health reasons and notes the core writes (`msg!`, English for the MCP
# agent), states the app words itself (containers, replica counts), and the
# labels of detail fields, which the app looks up by their English text.
# Data of the cluster (pod phases, condition messages, names) stays as it is.


## Health reasons of our own (`Health::phrase`)

state-active = Active
state-available = Available
state-ready = Ready
state-scaled-to-zero = Scaled to zero
state-rollout-paused = Rollout paused
state-waiting-rollout = Waiting for the rollout to be observed
state-waiting-controller = Waiting for the controller
state-waiting-status = Waiting for status
state-replicas-updated = { $updated } of { $desired } replicas updated
state-old-replicas = { $count } old replicas pending termination
state-updated-available = { $available } of { $updated } updated replicas available
state-replicas-ready = { $ready } of { $desired } replicas ready
state-pods-updated = { $updated } of { $desired } pods updated
state-pods-available = { $available } of { $desired } pods available
state-pods-misscheduled = { $count } pods misscheduled
state-hpa-disabled = Scaling disabled: target scaled to zero
state-hpa-waiting-metrics = Waiting for the first metrics
state-hpa-limited = Limited: { $reason }
state-hpa-scaling = Scaling
state-crd-established = Established
state-crd-not-established = Not established yet
state-gateway-accepted = Accepted
state-gateway-programmed = Programmed
state-gateway-listener = Listener { $name }: { $reason }
state-route-not-accepted = Not accepted by a gateway yet
state-route-waiting-gateways = Waiting for the gateways
state-deleting = Deleting
state-suspended = Suspended
state-job-failing = Failing
state-cronjob-active = { $count } active jobs
state-cronjob-last-failed = The last scheduled job did not succeed
state-cronjob-never-succeeded = No scheduled job has succeeded yet
state-cronjob-not-scheduled = Not scheduled yet
state-cronjob-scheduled = Scheduled
state-service-waiting-lb = Waiting for load balancer
state-ingress-waiting-address = Waiting for an address
state-node-scheduling-disabled = Ready, scheduling disabled
state-pod-containers-ready = { $ready }/{ $total } containers ready
state-quota-not-computed = Not computed yet
state-quota-within = Within quota
state-quota-at-limit = At the limit: { $resources }
state-pdb-not-observed = Not observed yet
state-pdb-no-pods = No pods selected
state-pdb-unhealthy = { $current } of { $desired } required pods healthy
state-pdb-no-disruptions = No disruptions allowed: evictions wait
state-pdb-disruptions = { $count } disruptions allowed
state-claim-protected = Terminating: kept by { $finalizer } while a pod uses the claim
state-volume-protected = Terminating: kept by { $finalizer } while a claim is bound
state-claim-used-by = Terminating: used by pods { $pods }
state-vpa-provided = Recommendation provided
state-vpa-provided-low = Recommendation provided (low confidence)
state-vpa-no-recommendation = No recommendation yet
state-container-running = Running
state-container-not-ready = Not ready
state-container-unknown = Unknown


## Notes of related objects (`Brief::phrased`)

brief-pod =
    { $ready }/{ $total } ready{ $restarts ->
        [0] {""}
       *[other] {" · "}{ $restarts } restarts
    }
brief-pod-on-node =
    { $ready }/{ $total } ready{ $restarts ->
        [0] {""}
       *[other] {" · "}{ $restarts } restarts
    } · { $node }
brief-revision-ready = revision { $revision } · { $ready }/{ $desired } ready
brief-ready = { $ready }/{ $desired } ready
brief-keys =
    { $count ->
        [one] { $count } key
       *[other] { $count } keys
    }


## States the app words itself

container-state-running = Running
container-state-waiting = Waiting: { $reason }
container-state-waiting-message = Waiting: { $reason } ({ $message })
container-state-terminated = Terminated: { $reason } (exit code { $code })
container-state-unknown = Unknown
container-state-not-ready = { $state }, not ready
replicas-desired = { $count } desired
replicas-current = { $count } current
replicas-ready = { $count } ready
replicas-updated = { $count } updated
replicas-available = { $count } available


## Numbered labels

field-rule-n = Rule { $n }
field-revision-of = Revision { $revision }

## Labels of detail fields, key/value rows and table columns (by their English text)

field-access-modes = Access modes
field-address = Address
field-addresses = Addresses
field-affinity = Affinity
field-aggregates = Aggregates
field-allowed-disruptions = Allowed disruptions
field-api-discovery-failed = API discovery failed
field-api-group-version-unreadable = API group version unreadable
field-api-groups = API groups
field-applications-sync-policy = Applications sync policy
field-architecture = Architecture
field-artifact-revision = Artifact revision
field-artifact-updated = Artifact updated
field-auto-sync = Auto-sync
field-backoff-limit = Backoff limit
field-binding-mode = Binding mode
field-capacity = Capacity
field-categories = Categories
field-chart = Chart
field-claim = Claim
field-class = Class
field-cluster-ips = Cluster IPs
field-completed = Completed
field-completions = Completions
field-concurrency-policy = Concurrency policy
field-container = Container
field-container-runtime = Container runtime
field-controller = Controller
field-conversion = Conversion
field-conversion-webhook = Conversion webhook
field-data-source = Data source
field-default = Default
field-default-backend = Default backend
field-delete = Delete
field-deleting-objects-failed = Deleting objects failed
field-deletion-requested = Deletion requested
field-description = Description
field-destination = Destination
field-duration = Duration
field-external-ip = External IP
field-external-traffic-policy = External traffic policy
field-extras = Extras
field-failure-policy = Failure policy
field-finalizers-remaining = Finalizers remaining
field-finished = Finished
field-from = From
field-generators = Generators
field-global-default = Global default
field-group = Group
field-health = Health
field-health-message = Health message
field-healthy-pods = Healthy pods
field-history-failed = History (failed)
field-history-successful = History (successful)
field-host-ip = Host IP
field-hostnames = Hostnames
field-immutable = Immutable
field-initiated-by = Initiated by
field-internal-traffic-policy = Internal traffic policy
field-interval = Interval
field-ip-families = IP families
field-kernel = Kernel
field-kind = Kind
field-kubelet = Kubelet
field-last-applied-revision = Last applied revision
field-last-attempted-revision = Last attempted revision
field-last-handled-reconcile = Last handled reconcile
field-last-scaled = Last scaled
field-last-schedule = Last schedule
field-last-successful = Last successful
field-limit = Limit
field-lower = Lower
field-match-conditions = Match conditions
field-match-policy = Match policy
field-max-replicas = Max replicas
field-max-unavailable = Max unavailable
field-message = Message
field-min-available = Min available
field-min-replicas = Min replicas
field-misscheduled = Misscheduled
field-mount-options = Mount options
field-name = Name
field-names = Names
field-namespace = Namespace
field-namespace-selector = Namespace selector
field-node = Node
field-node-selector = Node selector
field-object-selector = Object selector
field-objects = Objects
field-objects-remaining = Objects remaining
field-os-image = OS image
field-parallelism = Parallelism
field-parameters = Parameters
field-parent = Parent
field-path = Path
field-paused = Paused
field-phase = Phase
field-pod-cidr = Pod CIDR
field-pod-ip = Pod IP
field-pod-ips = Pod IPs
field-pod-management = Pod management
field-pod-selector = Pod selector
field-pods = Pods
field-policy-types = Policy types
field-preemption = Preemption
field-priority = Priority
field-project = Project
field-provider = Provider
field-provider-id = Provider ID
field-provisioner = Provisioner
field-prune = Prune
field-qos-class = QoS class
field-ready = Ready
field-reason = Reason
field-reclaim-policy = Reclaim policy
field-recommenders = Recommenders
field-reconciled-at = Reconciled at
field-ref = Ref
field-reference = Reference
field-reinvocation = Reinvocation
field-release-name = Release name
field-replicas = Replicas
field-repo = Repo
field-request = Request
field-resource = Resource
field-resource-names = Resource names
field-resources = Resources
field-restart-policy = Restart policy
field-retries = Retries
field-review-versions = Review versions
field-revision = Revision
field-roles = Roles
field-rule = Rule
field-rules = Rules
field-scale-down = Scale down
field-scale-up = Scale up
field-schedule = Schedule
field-schema = Schema
field-scope = Scope
field-scope-selector = Scope selector
field-scopes = Scopes
field-secret = Secret
field-selector = Selector
field-served = Served
field-service = Service
field-service-account = Service account
field-serviceaccount = ServiceAccount
field-session-affinity = Session affinity
field-short-names = Short names
field-side-effects = Side effects
field-source = Source
field-started = Started
field-starting-deadline = Starting deadline
field-status = Status
field-storage = Storage
field-storage-class = Storage class
field-stored-versions = Stored versions
field-strategy = Strategy
field-subjects = Subjects
field-suspend = Suspend
field-suspended = Suspended
field-sync-status = Sync status
field-synced-revision = Synced revision
field-target = Target
field-target-namespace = Target namespace
field-target-revision = Target revision
field-template-name = Template name
field-timeout = Timeout
field-timezone = Timezone
field-to = To
field-tolerations = Tolerations
field-total = Total
field-type = Type
field-types = Types
field-uncapped = Uncapped
field-unhealthy-pod-eviction = Unhealthy pod eviction
field-unschedulable = Unschedulable
field-update-mode = Update mode
field-update-strategy = Update strategy
field-upper = Upper
field-url = URL
field-value = Value
field-verbs = Verbs
field-version = Version
field-volume = Volume
field-volume-expansion = Volume expansion
field-volume-mode = Volume mode
field-webhooks = Webhooks

## Values of detail fields that are texts of ours (by their English text)

value-yes = Yes
value-no = No
value-not-allowed-kubernetes-keeps-this-namespace = Not allowed: Kubernetes keeps this namespace
value-system-namespace-deleting-it-asks-to-type-its-name = System namespace: deleting it asks to type its name
value-deleting-the-objects-inside-nothing-reported-as-stuck-yet = Deleting the objects inside; nothing reported as stuck yet
value-no-rule-all-ingress-is-denied = No rule: all ingress is denied
value-no-rule-all-egress-is-denied = No rule: all egress is denied
value-all-pods = all pods
value-all-namespaces = all namespaces

