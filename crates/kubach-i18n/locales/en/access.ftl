# Access control screens: Permissions (what I can do, who can) and Security
# checks of pods. Verbs (get, list), resources (pods, deployments.apps),
# RBAC kinds (Role, ClusterRoleBinding, ServiceAccount, User, Group), API
# groups, field names (hostNetwork, runAsUser) and errors from the cluster
# are data: keep them as they are.

## Permissions

access-tab-mine = What I can do
access-tab-who-can = Who can
access-check-again = Check again
access-pick-namespace = Pick a namespace
access-pick-namespace-description = Your permissions are reviewed for one namespace at a time.
access-reviewing = Reviewing your permissions…
access-cant-review = Can't review your permissions
# What the search found nothing among (ui-filter-no-matches).
access-noun-rules = rules
access-noun-subjects = subjects
access-noun-findings = findings
access-incomplete = The list may be incomplete. A missing rule doesn't mean access is denied.
# $error: why the API server could not evaluate every rule (English).
access-incomplete-reason = The list may be incomplete: { $error }. A missing rule doesn't mean access is denied.
# $urls: "/healthz (get) · /version (get)".
access-non-resource = Non-resource URLs: { $urls }
# $count: rules; $selected: how many namespaces are selected (the review
# runs in the first one when there are several).
access-rules-note =
    { $selected ->
        [0] { $count ->
            [one] { $count } rule for you in namespace { $namespace }, from SelfSubjectRulesReview
           *[other] { $count } rules for you in namespace { $namespace }, from SelfSubjectRulesReview
        }
        [1] { $count ->
            [one] { $count } rule for you in namespace { $namespace }, from SelfSubjectRulesReview
           *[other] { $count } rules for you in namespace { $namespace }, from SelfSubjectRulesReview
        }
       *[other] { $count ->
            [one] { $count } rule for you in namespace { $namespace } (the first of { $selected } selected), from SelfSubjectRulesReview
           *[other] { $count } rules for you in namespace { $namespace } (the first of { $selected } selected), from SelfSubjectRulesReview
        }
    }
access-column-verbs = Verbs
access-column-groups = API groups
access-column-names = Names
# The Names cell of a rule without resourceNames.
access-any-name = any

## Who can

access-verb-placeholder = verb
access-resource-placeholder = resource, e.g. deployments.apps
access-name-placeholder = any name
access-check = Check
access-who-can-invalid = Enter a verb and a resource, e.g. get pods.
access-checking = Checking…
access-checking-description = Listing roles and bindings of the cluster.
access-cant-check = Can't check access
# $error: the API server's message (English).
access-cant-check-description = { $error }. Listing roles and bindings needs list on the rbac.authorization.k8s.io resources cluster-wide.
# The question asked: "get pods named web".
access-question-named = { $verb } { $resource } named { $name }
# $question: "get pods" or access-question-named; $selected as in
# access-rules-note.
access-grants-note =
    { $selected ->
        [0] { $count ->
            [one] { $count } grant to { $question } in namespace { $namespace }. RBAC only: other authorizers and system:masters are not shown.
           *[other] { $count } grants to { $question } in namespace { $namespace }. RBAC only: other authorizers and system:masters are not shown.
        }
        [1] { $count ->
            [one] { $count } grant to { $question } in namespace { $namespace }. RBAC only: other authorizers and system:masters are not shown.
           *[other] { $count } grants to { $question } in namespace { $namespace }. RBAC only: other authorizers and system:masters are not shown.
        }
       *[other] { $count ->
            [one] { $count } grant to { $question } in namespace { $namespace } (the first of { $selected } selected). RBAC only: other authorizers and system:masters are not shown.
           *[other] { $count } grants to { $question } in namespace { $namespace } (the first of { $selected } selected). RBAC only: other authorizers and system:masters are not shown.
        }
    }
access-grants-note-cluster =
    { $count ->
        [one] { $count } grant to { $question } cluster-wide (only ClusterRoleBindings count). RBAC only: other authorizers and system:masters are not shown.
       *[other] { $count } grants to { $question } cluster-wide (only ClusterRoleBindings count). RBAC only: other authorizers and system:masters are not shown.
    }
access-nobody = Nobody is granted this by RBAC
access-view-table = Table
access-view-graph = Graph
access-graph-subjects = Subjects
access-graph-bindings = Bindings
access-graph-roles = Roles
access-graph-rules = Rules
access-subject = Subject
access-binding = Binding
access-role = Role
access-rule = Rule
# A rule: "get, list on pods, pods/log".
access-rule-label = { $verbs } on { $resources }
access-rule-label-named = { $verbs } on { $resources } named { $names }
access-rule-groups = API groups: { $groups }
access-open-binding = Open binding
access-open-role = Open role
access-open-service-account = Open service account

## Security checks

access-pods-not-served = Pods are not served
access-loading-pods = Loading pods…
access-no-access-pods = No access to pods
# $scope: any (all namespaces), one ($namespace) or many ($namespaces:
# how many).
access-no-access-pods-description =
    { $scope ->
        [one] Your credentials don't allow listing pods in namespace { $namespace }.
        [many] Your credentials don't allow listing pods in { $namespaces } namespaces.
       *[any] Your credentials don't allow listing pods in all namespaces.
    }
access-cant-load-pods = Can't load pods
access-no-findings = No findings
access-no-findings-description =
    { $scope ->
        [one] No pod in namespace { $namespace } breaks a check at this level.
        [many] No pod in { $namespaces } namespaces breaks a check at this level.
       *[any] No pod in all namespaces breaks a check at this level.
    }
# $summary: security-count-* joined, or security-no-issues.
access-security-summary =
    { $scope ->
        [one] { $summary } in namespace { $namespace }: privileged containers, hostPath, missing limits, :latest images and more
        [many] { $summary } in { $namespaces } namespaces: privileged containers, hostPath, missing limits, :latest images and more
       *[any] { $summary } in all namespaces: privileged containers, hostPath, missing limits, :latest images and more
    }
access-show-low = Show low severity
security-severity-high = High
security-severity-medium = Medium
security-severity-low = Low
security-count-high = { $count } high
security-count-medium = { $count } medium
security-count-low = { $count } low
security-no-issues = No issues
security-column-severity = Severity
security-column-pod = Pod
security-column-container = Container
security-column-finding = Finding
# The Container cell of a finding about the whole pod.
security-pod-level = pod
security-fix = Fix
security-open-pod = Open pod

## Findings (kubach-core)

# $field: hostNetwork, hostPID or hostIPC.
security-host-namespace = { $field } enabled
security-host-network-hint = Pod sees all host network interfaces. Remove hostNetwork unless strictly required.
security-host-pid-hint = Pod can see and signal host processes. Remove hostPID unless strictly required.
security-host-ipc-hint = Pod shares host IPC and shared memory. Remove hostIPC unless strictly required.
security-host-path = hostPath volume "{ $volume }"
security-host-path-hint = Mounts { $path } from the node. Use a PVC, configMap or emptyDir instead.
security-privileged = Privileged container
security-privileged-hint = Has full access to the host. Set privileged: false.
security-privilege-escalation = Privilege escalation allowed
security-privilege-escalation-hint = Set allowPrivilegeEscalation: false.
security-runs-as-root = Runs as root
security-runs-as-root-hint = runAsUser is 0. Use a non-zero UID.
security-may-run-as-root = May run as root
security-may-run-as-root-hint = Set runAsNonRoot: true or a non-zero runAsUser.
# $capability: SYS_ADMIN, NET_ADMIN…
security-capability = Adds capability { $capability }
security-capability-hint = Drop it unless strictly required; prefer drop: [ALL].
security-no-limits = No resource limits
security-no-limits-hint = Set memory and CPU limits so one pod cannot starve the node.
security-no-memory-limit = No memory limit
security-no-memory-limit-hint = Set a memory limit so one pod cannot starve the node.
security-no-requests = No resource requests
security-no-requests-hint = Set CPU and memory requests for predictable scheduling.
security-untagged-image = Untagged image
security-latest-image = Image uses :latest
security-mutable-tag-hint = { $image } may change under you. Pin a version tag or digest.
security-writable-root = Writable root filesystem
security-writable-root-hint = Set readOnlyRootFilesystem: true and mount writable paths explicitly.
