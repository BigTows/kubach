# Resource browser: the list of a kind with its filter bar, the details
# panel, the Pods and Connections blocks, the namespace picker. Kinds in
# their plural form ($title: "Deployments", "Pods"), names, namespaces and
# errors from the cluster are data: keep them as they are.

## List of a kind

# Before any kind is opened (the "← Back" button, the window title).
browser-resources = Resources
# The "← Back" target of a selected object. $kind: "Deployment".
browser-place-object = { $kind } { $name }
browser-search-placeholder = Search by name…
# The count above the table. $count: "12", or "3 of 12" (browser-shown-of);
# $scope: cluster (kind without namespace), any (all namespaces), one
# ($namespace) or many ($namespaces: how many).
browser-list-count =
    { $scope ->
        [any] { $count } { $title } in any namespace
        [one] { $count } { $title } in namespace { $namespace }
        [many] { $count } { $title } in { $namespaces } namespaces
       *[cluster] { $count } { $title }
    }
browser-shown-of = { $shown } of { $total }
# $age: how long ago, like "5m".
browser-cached-ago = cached { $age } ago

## Nothing to list

browser-no-cluster = No cluster connected
browser-connecting = Connecting…
browser-not-connected = Not connected
browser-discovering = Discovering API resources…
browser-discovery-failed = API discovery failed
browser-not-served = Not served
browser-not-served-description = This cluster doesn't serve this kind. Pick another in the sidebar.
browser-loading = Loading { $title }…
browser-no-access = No access to { $title }
# $error: the API server's message (English). $scope as in browser-list-count.
browser-no-access-description =
    { $scope ->
        [any] Your credentials don't allow listing { $title } in any namespace. { $error }
        [one] Your credentials don't allow listing { $title } in namespace { $namespace }. { $error }
        [many] Your credentials don't allow listing { $title } in { $namespaces } namespaces. { $error }
       *[cluster] Your credentials don't allow listing { $title }. { $error }
    }
browser-cant-load = Can't load { $title }
browser-lost-connection-retrying = Lost connection to the cluster. Retrying… ({ $error })
browser-error-retrying = { $error }. Retrying…
browser-empty = No { $title }
browser-nothing-found =
    { $scope ->
        [any] Nothing found in any namespace.
        [one] Nothing found in namespace { $namespace }.
        [many] Nothing found in { $namespaces } namespaces.
       *[cluster] Nothing found.
    }

## Banners over the table

browser-banner-connection-lost = Lost connection to the cluster, showing cached data. Retrying…
browser-banner-cant-refresh = Can't refresh { $title }, showing cached data. Retrying…
# $namespaces: the namespaces that failed, joined with commas.
browser-banner-no-access = No access to { $title } in { $namespaces }; showing the other namespaces.
browser-banner-cant-load = Can't load { $title } in { $namespaces }. Retrying…
# In the list of failed namespaces: a watch of the whole cluster.
browser-all-namespaces = all namespaces
browser-offline-error = Not connected, showing the last list from the cache: { $error }
browser-offline-disconnected = Disconnected, showing the last list from the cache.
browser-offline-connecting = Showing the last list from the cache while connecting…
# $names: the deleted namespaces, joined with commas.
browser-pruned =
    { $count ->
        [one] Namespace { $names } no longer exists; removed from the selection.
       *[other] Namespaces { $names } no longer exist; removed from the selection.
    }

## Bulk actions

browser-selected = Selected: { $count }
browser-no-common-action = No action applies to all selected objects
browser-clear-selection = Clear selection

## Namespace picker

picker-all-namespaces = All namespaces
# Shown instead of the selection for kinds without a namespace.
picker-cluster-scoped = Cluster-scoped
picker-no-permission = No permission to list namespaces; type a name.
picker-unavailable = Namespaces unavailable; type a name.

## Details panel

# $age: how long ago, like "5m".
details-ago = { $age } ago
# $place: "Deployment web", or the view the details were opened from.
details-back-tooltip = Back to { $place } (Esc)
details-evict-tooltip = Evict: through the Eviction API, respecting PodDisruptionBudgets
details-drain-tooltip = Drain…: cordon the node and evict its pods
details-node-shell-tooltip = Node shell: a privileged pod with a shell on the host
details-scale-down = Scale down by one
details-scale-up = Scale up by one
details-replicas =
    { $count ->
        [one] { $count } replica
       *[other] { $count } replicas
    }
details-scale-hpa = Managed by HPA { $name }: replicas will be overridden
details-scale-offline = Not connected: replicas shown are from the cache
details-scale-loading = Loading the live replica count…
details-scale-reconnecting = Reconnecting: replicas shown may be out of date
# A link to all objects of a CRD.
details-show-all = Show all { $kind } objects
details-open = Open { $kind } { $name }
details-name = Name
details-namespace = Namespace
details-created = Created
details-uid = UID
details-copy-name = Copy name
details-copy-uid = Copy UID
details-reason = Reason
details-security = Security
details-owner = Owner
details-managed-by = Managed by
details-labels = Labels
details-annotations = Annotations
details-loading = Loading…
# $error: why the list failed (English, from the cluster).
details-unavailable = Unavailable: { $error }
details-none = None
# An object a section expects (a ConfigMap of a pod) that does not exist.
details-not-created-yet = not created yet
details-not-served = the cluster does not serve { $kind }
details-not-found = Not found: the object is gone.
details-no-access = No access to objects of this kind here.
details-cant-load = Can't load it: { $error }

## Containers in the details

details-container-ready = ready
details-container-not-ready = not ready
details-container-restarts =
    { $count ->
        [one] { $count } restart
       *[other] { $count } restarts
    }
details-image = Image
details-state = State
details-last-state = Last state
details-ports = Ports
details-requests = Requests
details-limits = Limits
details-environment = Environment ({ $count })
details-mounts = Mounts ({ $count })

## Container ports

# $protocol: UDP, SCTP.
details-port-tcp-only = { $protocol }: port forward supports TCP only
# $address: 127.0.0.1:8080.
details-port-forwarded-open = Forwarded to { $address }. Click to open { $url }
details-port-forwarded-copy = Forwarded to { $address }. Click to copy the address
details-port-forward = Port forward { $port }: pick the local port
details-port-open = Open { $url }
details-copy-address = Copy address
details-stop-forward = Stop port forward

## Runs started from the details

details-run-scale-down = Scale down { $name }
details-run-scale-up = Scale up { $name }
details-action-interrupted = The action was interrupted
node-drain-run = Drain { $node }
node-drain-cordoning = Cordoning and listing pods…
node-drain-no-pods = No pods to evict
node-drain-finishing = Finishing…
node-drain-evicting =
    { $count ->
        [one] Evicting: { $count } pod left
       *[other] Evicting: { $count } pods left
    }
# $error: why the drain stopped (English).
node-drain-aborted = Drain aborted: { $error }

## Pods of a workload

pods-none = No pods
pods-not-scheduled = Not scheduled

## Namespace contents

namespace-contents = Contents
namespace-counting = Counting…
namespace-no-permission = no permission
namespace-not-counted = not counted: { $error }
namespace-contents-empty = No pods, workloads, services, config or claims
namespace-count-again = Count again

## "Managed by" block

managed-by-missing = not found in this cluster
managed-by-not-served = kind not installed in this cluster
managed-by-failed = can't check: { $error }
# $via: what tells the manager (a label or annotation).
managed-by-open = Open · from { $via }
# $note: managed-by-missing, -not-served or -failed.
managed-by-note = { $note } · from { $via }

## Connections block

connections-title = Connections
connections-none = Not connected to other objects
