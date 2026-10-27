# Texts the built-in resource plugins of kubach-core give the UI: column
# headers, action names, detail section titles and health states. The core
# keeps them in English (the MCP agent reads them); the app translates them
# by id here. Unknown ids (CRD printer columns) keep the core's text.


## Column headers

column-access-modes = Access modes
column-active = Active
column-address = Address
column-addresses = Addresses
column-age = Age
column-allowed = Allowed disruptions
column-applications = Applications
column-available = Available
column-backends = Backends
column-binding-mode = Binding mode
column-capacity = Capacity
column-chart = Chart
column-claim = Claim
column-class = Class
column-cluster-ip = Cluster IP
column-completions = Completions
column-containers = Containers
column-controller = Controller
column-cpu = CPU
column-current = Current
column-data = Data
column-default = Default
column-desired = Desired
column-destination = Destination
column-duration = Duration
column-egress = Egress
column-expansion = Expansion
column-external-ip = External IP
column-failure-policy = Failure policy
column-from = From
column-generators = Generators
column-global-default = Global default
column-group = Group
column-health = Health
column-healthy = Healthy
column-hostnames = Hostnames
column-hosts = Hosts
column-images = Images
column-ingress = Ingress
column-internal-ip = Internal IP
column-ip = IP
column-kind = Kind
column-labels = Labels
column-last-schedule = Last schedule
column-listeners = Listeners
column-max = Max
column-max-unavailable = Max unavailable
column-memory = Memory
column-min = Min
column-min-available = Min available
column-mode = Mode
column-most-used = Most used
column-name = Name
column-namespace = Namespace
column-node = Node
column-node-selector = Node selector
column-parameters = Parameters
column-parents = Parents
column-path = Path
column-pod-selector = Pod selector
column-policy-types = Policy types
column-ports = Ports
column-preemption = Preemption
column-project = Project
column-provisioner = Provisioner
column-ready = Ready
column-reclaim-policy = Reclaim policy
column-ref = Ref
column-reference = Reference
column-replicas = Replicas
column-repo = Repo
column-request = Request
column-resources = Resources
column-restarts = Restarts
column-revision = Revision
column-role = Role
column-roles = Roles
column-rules = Rules
column-runtime = Container runtime
column-schedule = Schedule
column-scope = Scope
column-selector = Selector
column-source = Source
column-status = Status
column-storage-class = Storage class
column-subjects = Subjects
column-suspend = Suspend
column-suspended = Suspended
column-sync = Sync status
column-target = Target
column-targets = Targets
column-timezone = Timezone
column-to = To
column-type = Type
column-types = Types
column-up-to-date = Up-to-date
column-url = URL
column-value = Value
column-version = Version
column-versions = Versions
column-volume = Volume
column-webhooks = Webhooks

## Actions

action-delete = Delete
action-scale = Scale
action-restart = Rollout restart
action-trigger = Trigger now
action-suspend = Suspend
action-resume = Resume
action-cordon = Cordon
action-uncordon = Uncordon
action-evict = Evict
action-logs = Logs
action-exec = Exec
action-port-forward = Port forward
action-debug = Debug
action-refresh = Refresh
action-hard-refresh = Hard refresh
action-sync = Sync
action-reconcile = Reconcile

## Detail sections

section-active-jobs = Active jobs
section-addresses = Addresses
section-allocatable = Allocatable
section-allowed-topologies = Allowed topologies
section-annotations = Annotations
section-applications = Applications
section-backends = Backends
section-behavior = Behavior
section-binary-data = Binary data
section-capacity = Capacity
section-conditions = Conditions
section-container-policies = Container policies
section-containers = Containers
section-data = Data
section-deletion-blocked-by = Deletion blocked by
section-diagnostics = Diagnostics
section-disrupted-pods = Disrupted pods
section-egress-rules = Egress rules
section-finalizers = Finalizers
section-from = From
section-history = History
section-images = Images
section-ingress-rules = Ingress rules
section-init-containers = Init containers
section-inventory = Inventory
section-labels = Labels
section-listeners = Listeners
section-metrics = Metrics
section-operation = Operation
section-owner-references = Owner references
section-parameters = Parameters
section-ports = Ports
section-recommendations = Recommendations
section-resources = Resources
section-resources-summary = Resources summary
section-rules = Rules
section-rules-aggregated = Rules (aggregated)
section-scheduling = Scheduling
section-security = Security
section-selector = Selector
section-source = Source
section-sources = Sources
section-subjects = Subjects
section-sync-history = Sync history
section-sync-result = Sync result
section-system-info = System info
section-taints = Taints
section-template-sources = Template sources
section-to = To
section-usage = Usage
section-used-by-pods = Used by pods
section-versions = Versions
section-volume-claim-templates = Volume claim templates
section-volumes = Volumes

## Health states

health-healthy = Healthy
health-progressing = Progressing
health-degraded = Degraded
health-failed = Failed
health-unknown = Unknown

## Section titles with data

# $item: a LimitRange item type (Container, Pod, PersistentVolumeClaim).
section-limits = { $item } limits
# $parent: the parent reference of a route (Gateway web/main).
section-parent = Parent { $parent }
