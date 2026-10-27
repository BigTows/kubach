# Built-in text of the design system components (kubach-ui). Names and
# values passed in by the app (Kubernetes kinds, states, context names)
# are data: keep them as they are.

## Shared

ui-cancel = Cancel
# A list cut after a few items: "and 3 more".
ui-and-more = and { $count } more
# A value that could not be read (metrics).
ui-unavailable = unavailable

## Action toast (a running action with a stop button)

ui-action-stop = Stop
ui-action-stopping = Stopping…

## Bulk progress (one action over many objects)

ui-bulk-running = Running…
ui-bulk-done = Done
ui-bulk-cancelled = Cancelled
ui-bulk-failed = Failed
ui-bulk-succeeded = { $succeeded } succeeded
ui-bulk-succeeded-failed = { $succeeded } succeeded, { $failed } failed
ui-bulk-hide-tooltip = Hide; it keeps running in the status bar
ui-bulk-hide-output = Hide output
ui-bulk-show-output = Show output ({ $count })

## Chips (labels and annotations)

# Second line of a chip tooltip; the first one is the key=value.
ui-chip-click-to-copy = Click to copy
ui-chip-hide = Hide
ui-chip-show-less = Show less
ui-chip-show-all = Show { $count }
ui-chip-more = +{ $count } more
ui-chip-copy-all = Copy all

## Confirm dialog

ui-confirm = Confirm
ui-confirm-command = Command
ui-confirm-copy-command = Copy command
# $text: the name the user has to type, e.g. the namespace being deleted.
ui-confirm-type-to-confirm = Type { $text } to confirm.
ui-confirm-context = Context

## Diff

ui-diff-unchanged-lines =
    { $count ->
        [one] ⋯ { $count } unchanged line
       *[other] ⋯ { $count } unchanged lines
    }

## Search in lists

# $noun: what the list shows, from the app ("Pods", "releases").
ui-filter-no-matches = No { $noun } match “{ $query }”
ui-filter-no-matches-description = Search looks for the text anywhere in the name, ignoring case.
ui-filter-clear-search = Clear search

## Traffic graph

ui-traffic-low = Low
ui-traffic-medium = Medium
ui-traffic-high = High
ui-traffic-degraded = Errors ≥1%
ui-traffic-failing = Errors ≥5%
# Traffic to Pods that no Service selects.
ui-traffic-no-service = No Service
ui-graph-group-items =
    { $count ->
        [one] { $count } item
       *[other] { $count } items
    }
ui-graph-zoom-out = Zoom out
ui-graph-actual-size = Actual size
ui-graph-zoom-in = Zoom in
ui-graph-fit = Fit to view
ui-graph-minimap = Minimap
ui-graph-laying-out = Laying out…
ui-graph-click-to-pan = Click to pan and zoom

## Hotbar tile colors

ui-tile-blue = Blue
ui-tile-teal = Teal
ui-tile-orange = Orange
ui-tile-purple = Purple
ui-tile-pink = Pink
ui-tile-gray = Gray
ui-tile-custom = Custom

## Charts

# Right edge of a time axis; the other ticks are "-5m", "-1h".
ui-chart-now = now
ui-chart-no-data = No data

## Log view

ui-log-no-lines = No lines yet
ui-log-new-lines =
    { $count ->
        [one] { $count } new line
       *[other] { $count } new lines
    }

## Multi-select (namespaces and similar)

ui-multi-none = None
ui-multi-filter = Filter…
ui-multi-all = All
ui-multi-select = Select…
ui-multi-recent = Recent
# Adds the typed text as an item.
ui-multi-use = Use “{ $value }”
ui-multi-no-matches = No matches
ui-multi-hint = Applies as you pick · ⌥-click for only one

## Operations (running Helm changes, drains, bulk actions)

ui-operations-title = Operations
ui-operations-empty = No operations yet
ui-operations-empty-description = Helm changes, drains and bulk actions show up here while they run.
ui-operations-clear-finished = Clear finished
ui-operations-notice-tooltip = Show operations

## Tables and bars

ui-replicas-ready = { $ready }/{ $desired } ready
# $age: kubectl-style age, e.g. "5m" (restarts and the last one).
ui-table-count-age = { $count } ({ $age } ago)
ui-table-reset-columns = Reset column order
ui-cubes-restarts = Restarts: { $count }
ui-side-list-empty = Nothing here
ui-usage-allocatable = Allocatable
ui-usage-used = Used
ui-usage-requests = Requests
ui-usage-limits = Limits

## Secrets and settings fields

ui-secret-choose-file = Choose file…
ui-secret-no-file = No file chosen
ui-secret-length = Length
ui-secret-generate = Generate
ui-setting-reset = Reset to default
ui-setting-not-set = Not set
ui-setting-choose = Choose…
