# Actions on objects: confirmations, the toasts of their runs, scale to 0,
# Drain and Shell for a Node. $action is the action's name (action-* in
# columns.ftl), $kind a kind ("Pod"), $title its plural ("Pods"), $object
# "namespace/name". Names, kinds, flags and kubectl commands are data: keep
# them as they are. $scope names where the objects are, as in
# browser-list-count: cluster, any (namespace), one ($namespace), many
# ($namespaces of them).

## Confirmation of an action

# One object, from the details panel.
action-confirm-title-one = { $action } { $kind } { $name }?
action-confirm-title-many = { $action } { $count } { $title }?
# The confirm button for several objects.
action-confirm-label-many = { $action } { $count }
action-confirm-one = { $action } { $object }?
action-confirm-many =
    { $scope ->
        [any] { $action } { $count } { $title } in any namespace?
        [one] { $action } { $count } { $title } in namespace { $namespace }?
        [many] { $action } { $count } { $title } in { $namespaces } namespaces?
       *[cluster] { $action } { $count } { $title }?
    }
action-delete-one = This permanently deletes { $object }.
action-delete-many =
    { $scope ->
        [any] This permanently deletes { $count } { $title } in any namespace.
        [one] This permanently deletes { $count } { $title } in namespace { $namespace }.
        [many] This permanently deletes { $count } { $title } in { $namespaces } namespaces.
       *[cluster] This permanently deletes { $count } { $title }.
    }
action-evict-one = Evict pod { $object } via the Eviction API (respects PodDisruptionBudgets).
action-evict-many =
    { $count ->
        [one] Evict { $count } pod via the Eviction API (respects PodDisruptionBudgets).
       *[other] Evict { $count } pods via the Eviction API (respects PodDisruptionBudgets).
    }
action-trigger-one = Creates a Job from CronJob { $object } now, outside its schedule.
action-trigger-many =
    { $count ->
        [one] Creates a Job from { $count } CronJob now, outside its schedule.
       *[other] Creates a Job from each of { $count } CronJobs now, outside their schedules.
    }
action-cordon-many =
    { $count ->
        [one] No new pods are scheduled on this node until it is uncordoned.
       *[other] No new pods are scheduled on these { $count } nodes until they are uncordoned.
    }
# $pods: pod names, $finalizer: "kubernetes.io/pvc-protection".
action-claim-used-by = Used by pods { $pods }: the claim stays Terminating ({ $finalizer }) until they are deleted.
# One claim of several: its name, then what action-claim-used-by says.
action-claim-of = { $object }: { $note }
action-claim-maybe-used = A claim that a pod still mounts stays Terminating ({ $finalizer }) until the pod is deleted.
# Delete of a namespace. $contents: a list of action-namespace-count.
action-namespace-holds = { $namespace } holds { $contents }. Deleting a namespace deletes every object inside it, and that cannot be undone.
action-namespace-empty = { $namespace } has none of the main kinds (pods, workloads, services, config, claims). Deleting a namespace deletes every object inside it, and that cannot be undone.
action-namespace-unknown = Everything in { $namespace } is deleted with it. Deleting a namespace deletes every object inside it, and that cannot be undone.
# $kind: singular ("Pod"), $title: plural ("Pods").
action-namespace-count =
    { $count ->
        [1] 1 { $kind }
       *[other] { $count } { $title }
    }

## Run of an action (toast)

# $kind for one object, $title for several.
action-run-title =
    { $count ->
        [1] { $action } 1 { $kind }
       *[other] { $action } { $count } { $title }
    }
# $error: why the run stopped (English).
action-run-aborted = Bulk run aborted: { $error }
action-rbac-hint = Kubernetes RBAC refused some objects (HTTP 403): your credentials lack this permission there. Check with kubectl auth can-i or ask a cluster admin.
# $error: the API server's message (English).
action-error-forbidden = No permission (RBAC): { $error }
action-error-not-found = Not found: { $error }
# An error that has only its own text (English, from the API server).
action-error-text = { $error }

## Scale to 0

action-scale-zero-title = Scale { $kind } { $name } to 0?
action-scale-zero-message = This stops every pod of { $object }; it serves nothing until scaled up again.
action-scale-zero-confirm = Scale to 0

## Drain a Node

drain-confirm-title = Drain Node { $node }?
# $minutes: how long refusals of PodDisruptionBudgets are retried.
drain-confirm-message =
    { $minutes ->
        [one] Cordons { $node }, then evicts its pods through the Eviction API: PodDisruptionBudgets are respected and their refusals retried for up to { $minutes } minute. DaemonSet and static pods stay.
       *[other] Cordons { $node }, then evicts its pods through the Eviction API: PodDisruptionBudgets are respected and their refusals retried for up to { $minutes } minutes. DaemonSet and static pods stay.
    }
# Appended to drain-confirm-message. $error: why (English).
drain-list-failed = Couldn't list the node's pods ({ $error }); the drain lists them again when it runs.
drain-confirm = Drain
drain-option-ignore-daemonsets = Ignore DaemonSets (--ignore-daemonsets)
drain-option-delete-emptydir = Delete emptyDir data (--delete-emptydir-data)
drain-option-force = Force: pods without a controller (--force)
drain-blocked-notice = These pods stop the drain; check the options they need:
drain-emptydir-notice = Data in emptyDir volumes of the evicted pods is lost.
# One pod of the plan: "namespace/name — what happens to it" (drain-verdict-*,
# or for blocked pods drain-block-*).
drain-plan-line = { $pod } — { $what }
drain-verdict-evict = evict
drain-verdict-delete = delete (finished)
drain-verdict-wait = already terminating
drain-verdict-skip = skip: { $reason }
drain-verdict-blocked = blocked: { $reason }
# Why a pod stays on the node.
drain-skip-mirror = static (mirror) pod
drain-skip-daemonset = DaemonSet pod
# Why a pod stops the drain: the option to check.
drain-block-daemonset = DaemonSet pod: check Ignore DaemonSets
drain-block-no-controller = not managed by a controller: check Force
drain-block-emptydir = uses emptyDir (data is lost): check Delete emptyDir data
# After a drain. $pods: a list of drain-skipped-*.
drain-left-on-node = Left on the node: { $pods }.
drain-skipped-mirror =
    { $count ->
        [one] { $count } static (mirror) pod
       *[other] { $count } static (mirror) pods
    }
drain-skipped-daemonset =
    { $count ->
        [one] { $count } DaemonSet pod
       *[other] { $count } DaemonSet pods
    }
# Why the drain or a pod of it failed. $error: the API server's message.
drain-cordon-failed = Cordon failed: { $error }
drain-list-pods-failed = Listing pods failed: { $error }
drain-cancelled-evicting = Cancelled while evicting
drain-eviction-timeout = The eviction was still unanswered when the drain timeout ran out
drain-gave-up = { $error }; gave up when the drain timeout ran out
drain-cancelled = Cancelled: { $error }
drain-terminating-timeout = Evicted, but still terminating when the drain timeout ran out
drain-cancelled-waiting = Cancelled while waiting for the pod to terminate

## Shell on a Node

node-shell-confirm-title = Shell on Node { $node }?
node-shell-confirm-message = Kubach creates a privileged pod on { $node } in namespace { $namespace } (host PID, network and IPC, tolerates every taint) and opens a root shell on the host through nsenter. The pod is deleted when the tab closes. Namespace and image are in Settings → General → Terminal.
node-shell-image = Image: { $image }
node-shell-root-notice = The shell is root on the node: it can read every pod's data and change the host.
node-shell-confirm = Open shell
