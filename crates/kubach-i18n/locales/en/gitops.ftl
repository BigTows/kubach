# GitOps screen: Argo CD Applications, Flux Kustomizations, HelmReleases
# and sources. Sync and health states (Synced, OutOfSync, Healthy), kinds,
# names, revisions and errors are data: keep them as they are.

## Attention levels (status column and badge)

gitops-attention-ok = OK
gitops-attention-unknown = Unknown
gitops-attention-suspended = Suspended
gitops-attention-pending = In progress
gitops-attention-warning = Warning
gitops-attention-error = Error

## List

gitops-problems-only = Problems only
# $count: how many objects are listed.
gitops-objects =
    { $count ->
        [one] { $count } object
       *[other] { $count } objects
    }
# Counts next to the total, after a number.
gitops-count-failing = failing
gitops-count-out-of-sync = out of sync
gitops-count-in-progress = in progress
gitops-count-suspended = suspended
gitops-count-healthy = healthy
# A kind that can't be listed. $kind: "Application", $error: the API
# server's message (English).
gitops-list-error = { $kind }s: { $error }
gitops-list-error-in = { $kind }s in { $namespace }: { $error }
# $errors: gitops-list-error entries joined with "; ".
gitops-cant-list-kinds = Can't list { $errors }
# What the search found nothing among (ui-filter-no-matches).
gitops-noun-problems = problems
gitops-noun-objects = GitOps objects
gitops-no-problems = No problems
gitops-no-problems-description = Everything is synced, reconciled or in progress.

## Nothing to list

gitops-no-tools = No GitOps tools
gitops-no-tools-description = This cluster serves neither Argo CD nor Flux.
gitops-cant-list = Can't list GitOps objects
gitops-empty = No Applications, Kustomizations or HelmReleases
gitops-empty-any-namespace = Nothing in any namespace.
gitops-empty-selected-namespaces = Nothing in the selected namespaces. Argo CD Applications usually live in argocd, Flux objects in flux-system.

## Details

# Under the name of a cluster-scoped object. $kind: "Flux Kustomization".
gitops-cluster-scoped = { $kind } · cluster
# Badges. $state: the sync, ready or health state ("OutOfSync", "True").
gitops-badge-sync = Sync: { $state }
gitops-badge-ready = Ready: { $state }
gitops-badge-health = Health: { $state }
gitops-errors = Errors
gitops-overview = Overview
gitops-revision = Revision
gitops-attempted-revision = Attempted revision
gitops-destination = Destination
gitops-ref = Ref
gitops-target-namespace = Target namespace
gitops-url = URL
gitops-source = Source
gitops-source-object = Source object
gitops-status-changed = Status changed
gitops-suspended = Suspended
gitops-managed = Managed resources · { $count }
