# Events: the Events screen and the Events panel in the details of an
# object. Reasons (BackOff), messages and kinds of events are data from the
# cluster: keep them as they are.

events-title = Events
events-title-warnings =
    { $count ->
        [one] Events · { $count } warning
       *[other] Events · { $count } warnings
    }
events-loading = Loading events…
events-unavailable = Events unavailable
events-none = No events
events-none-description = Nothing happened here recently.
# Why the panel has no events (shown as an error).
events-not-served-error = the cluster serves no Event API
events-not-served = This cluster serves no Event API.
events-search-placeholder = Search by object name…
events-warnings-only = Warnings only
events-no-access = No access to events
events-cant-load = Can't load events
events-summary =
    { $count ->
        [one] { $count } event
       *[other] { $count } events
    } · { $warnings ->
        [one] { $warnings } warning
       *[other] { $warnings } warnings
    }
events-summary-capped =
    { $count ->
        [one] { $count } event
       *[other] { $count } events
    } · { $warnings ->
        [one] { $warnings } warning
       *[other] { $warnings } warnings
    } · showing the latest { $shown }
# What the search found nothing among (ui-filter-no-matches).
events-noun = events
events-no-warnings = No warnings
events-new =
    { $count ->
        [one] { $count } new event
       *[other] { $count } new events
    }
events-new-tooltip = Scroll to the newest events
