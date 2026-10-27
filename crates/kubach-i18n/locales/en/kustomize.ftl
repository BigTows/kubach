# Kustomize: building a kustomization folder, the objects it produces and
# the comparison with the cluster. Folder paths, kinds, object names, API
# versions and the output of kustomize and kubectl are data. Kubernetes terms
# (kind, namespace, Secret, kustomization, helmCharts) stay in English.

kustomize-description = Build a kustomization folder and compare it with the cluster.
kustomize-recent = Recent
kustomize-recent-tooltip = Recent folders
kustomize-enable-helm-tooltip = Inflate helmCharts (needs helm on PATH)
kustomize-rebuild-tooltip = Build again
kustomize-open-folder = Open folder…
kustomize-no-folder = No folder open
kustomize-no-folder-description = Pick a folder with kustomization.yaml. Kubach runs `kustomize build` (or `kubectl kustomize`) locally; nothing is applied.
kustomize-building = Building…

## Build errors

kustomize-error-no-tool = kustomize is not available
kustomize-error-no-tool-hint = Install kustomize or kubectl and make sure it is on PATH (Kubach reads PATH from your login shell).
kustomize-error-no-tool-details = neither `kustomize` nor `kubectl` was found in PATH
kustomize-error-not-kustomization = Not a kustomization folder
kustomize-error-not-kustomization-hint = Pick a folder that contains kustomization.yaml, kustomization.yml or Kustomization.
kustomize-error-not-kustomization-details = { $path } has no kustomization.yaml
kustomize-error-spawn = Can't run kustomize
kustomize-error-spawn-hint = Check that the binary exists and is executable.
kustomize-error-spawn-details = failed to run { $program }: { $error }
kustomize-error-timeout = Build timed out
kustomize-error-timeout-hint = Remote bases and helmCharts need network access; check the URLs.
kustomize-error-timeout-details = kustomize build did not finish within { $seconds } s
kustomize-error-failed = Build failed
kustomize-error-unreadable = Unreadable build output
kustomize-error-not-utf8 = kustomize output is not UTF-8
kustomize-error-yaml = invalid kustomize output: { $error }
kustomize-error-interrupted = Build was interrupted

## Objects

kustomize-column-kind = Kind
kustomize-column-name = Name
kustomize-column-namespace = Namespace
kustomize-column-api-version = API version
kustomize-column-cluster = In cluster
kustomize-status-new = New
kustomize-status-unchanged = Unchanged
kustomize-status-changed = Changed
kustomize-status-error = Error
kustomize-tab-resources = Resources
kustomize-tab-output = Output
kustomize-tab-diff = Diff with cluster
# Parts of the line over the list, joined by " · ".
kustomize-objects =
    { $count ->
        [one] { $count } object
       *[other] { $count } objects
    }
kustomize-compare-connect = connect to a cluster to compare
kustomize-compare-running = comparing with the cluster…
# $counts is kustomize-count-* joined by " · ".
kustomize-compare-result = cluster: { $counts }
kustomize-built-with = built with { $tool }
kustomize-count-new = { $count } new
kustomize-count-changed = { $count } changed
kustomize-count-unchanged = { $count } unchanged
kustomize-count-failed = { $count } not compared
kustomize-count-nothing = nothing to compare
kustomize-empty-build = Empty build
kustomize-empty-build-description = The kustomization produced no objects.
kustomize-secrets-hidden = Secret values hidden
kustomize-secrets-hidden-tooltip = Secret values are hidden on this context by its security settings.
kustomize-secrets-hide = Hide secret values
kustomize-secrets-reveal = Reveal secret values

## Comparison with the cluster

kustomize-compare-error = Can't compare
kustomize-comparing = Comparing with the cluster…
kustomize-no-cluster = No cluster connected
kustomize-no-cluster-description = Connect to a cluster to compare the build with it.
kustomize-no-differences = No differences
kustomize-no-differences-description = The live object already has every field the build sets.
kustomize-diff-new = Not in the cluster yet: applying creates it.
kustomize-diff-fields =
    { $count ->
        [one] { $count } field differs. Only fields set by the build are compared; server defaults and status are left out.
       *[other] { $count } fields differ. Only fields set by the build are compared; server defaults and status are left out.
    }
