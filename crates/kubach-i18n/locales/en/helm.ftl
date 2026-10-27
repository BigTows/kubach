# Helm: the Releases and Charts pages, release details, rollback, uninstall,
# upgrade and install, OCI chart versions, Settings → External tools (the
# helm binary and the copy Kubach downloads), and why a change asked in one
# context did not run. Release, chart, repository, namespace and context
# names, versions, statuses (deployed, pending-upgrade), helm commands and
# helm's own output are data: keep them as they are. Kubernetes and Helm
# terms (Helm, release, chart, values, manifest, hook, Secret, ConfigMap,
# PVC, PersistentVolumeClaim, CRD, OCI) stay in English.

## Pages

helm-page-releases = Helm releases
helm-page-charts = Helm charts
helm-no-cluster = No cluster connected
helm-connecting = Connecting…
helm-not-connected = Not connected
helm-loading = Loading…
helm-cancel = Cancel
helm-back = Back
helm-dismiss = Dismiss

## Release list

helm-reload-releases = Reload releases
helm-reloading-releases = Reloading releases…
helm-search-releases = Search releases by name…
# The list in "No … match": lowercase.
helm-noun-releases = releases
# Where releases are stored or looked for; $kind is Secret or ConfigMap.
helm-driver-detected = stored in { $kind }s
helm-driver-settings = stored in { $kind }s, per settings
helm-driver-default = looked up in { $kind }s
# Which namespaces the list shows; $scope is all, one or many.
helm-releases-scope =
    { $scope ->
        [all] in any namespace
        [one] in namespace { $namespace }
       *[many] in { $namespaces } namespaces
    }
helm-releases-loading = Loading Helm releases…
helm-releases-list-error = Can't list Helm releases
# A namespace list entry for a failure in all namespaces at once.
helm-all-namespaces = all namespaces
helm-releases-forbidden = No access to Helm release { $kind }s in { $namespaces }: your credentials can't list { $kind }s there.
helm-releases-list-failed = Can't list releases in { $namespaces }: { $error }
helm-releases-none = No Helm releases
# $scope is helm-releases-scope, $driver one of helm-driver-*.
helm-releases-none-description = Nothing found { $scope } ({ $driver }). The storage driver can be set per context in Settings → Clusters.
helm-releases-none-shown = No Helm releases shown
helm-releases-summary =
    { $count ->
        [one] { $count } release { $scope } · { $driver }
       *[other] { $count } releases { $scope } · { $driver }
    }
helm-releases-summary-filtered =
    { $count ->
        [one] { $shown } of { $count } release { $scope } · { $driver }
       *[other] { $shown } of { $count } releases { $scope } · { $driver }
    }
helm-cluster-serves-no = the cluster serves no { $kind }s
helm-listing-interrupted = Listing was interrupted: { $error }

## Release details

helm-back-releases = Releases
helm-release-loading = Loading release…
helm-release-load-error = Can't load the release
helm-release-not-found = Release not found
helm-loading-interrupted = Loading was interrupted: { $error }
helm-revision = Revision { $revision }
helm-revision-latest = { $revision } (latest)
helm-revision-of = { $revision } of { $latest }
helm-field-status = Status
helm-field-revision = Revision
helm-field-chart = Chart
helm-field-version = Version
helm-field-app-version = App version
helm-field-updated = Updated
helm-field-first-deployed = First deployed
helm-field-description = Description
helm-field-labels = Labels
helm-field-repository = Repository
helm-field-home = Home
helm-field-sources = Sources
helm-field-keywords = Keywords
helm-field-maintainers = Maintainers
helm-field-created = Created
helm-column-name = Name
helm-column-namespace = Namespace
helm-showing-old-revision = Showing revision { $revision } — not the current state of the release.
helm-show-latest = Show latest
helm-update-available = New version { $version } available in repo { $repository }
helm-update-available-app = New version { $version } (app { $app }) available in repo { $repository }
helm-upgrade-to-tooltip = Upgrade to { $version } from { $repository }
helm-tab-values = Values
helm-tab-computed-values = Computed values
helm-tab-manifest = Manifest
helm-tab-hooks = Hooks
helm-tab-notes = Notes
helm-tab-history = History
helm-tab-diff = Diff
helm-hidden-secrets = Hidden: may contain secrets
helm-hidden-secrets-description = Values, manifests and hooks often carry passwords and tokens. Reveal them only where nobody is looking over your shoulder.
helm-secret-values-hidden = Secret values are hidden on this context by its security settings.
helm-reveal = Reveal
helm-notes-none = No notes
helm-notes-none-description = The chart has no NOTES.txt.
helm-history-hint = Pick a revision to view it in the other tabs or roll back to it.
# Comment lines over the YAML of the Hooks and Computed values tabs.
helm-values-warning = warning: { $warning }
helm-hooks-none = This revision has no hooks.
helm-hook-header = { $kind } { $name } · { $events } · weight { $weight }
helm-hook-delete-policy = delete policy: { $policies }
helm-hook-last-run = last run: { $phase }

## Diff tab

helm-diff-nothing = Nothing to compare
helm-diff-single-revision = The release has a single revision.
# Between the two revision pickers: "from [Revision 2] to [Revision 3]".
helm-diff-from = from
helm-diff-to = to
helm-diff-none = No differences
# $source is a tab name: Manifest, Values or Computed values.
helm-diff-identical = { $source } of revisions { $from } and { $to } are identical.

## Rollback, upgrade, uninstall

helm-rollback-button = Roll back to { $revision }
helm-rollback-title = Roll back { $name } to revision { $revision }?
helm-rollback-message = Helm creates revision { $next } of { $name } in { $namespace } with the manifest and values of revision { $revision }; the chart's rollback hooks run.
helm-rollback-confirm = Roll back
helm-rollback-toast = Rollback of { $name } to revision { $revision }
helm-upgrade-button = Upgrade…
helm-upgrade-tooltip = Edit values, preview the manifest diff and upgrade
helm-upgrade-title = Upgrade { $name }?
helm-upgrade-confirm-message = Helm creates a new revision of { $name } in { $namespace } as previewed.
helm-upgrade-confirm-message-moved = The release changed after revision { $revision }; the preview may be out of date. Helm creates a new revision of { $name } in { $namespace } as previewed.
helm-upgrade-confirm = Upgrade
helm-upgrade-toast = Upgrade of { $name }
helm-upgrade-release-moved = The release changed since revision { $revision }; the preview may be out of date.
helm-uninstall-button = Uninstall…
helm-uninstall-title = Uninstall { $name }?
helm-uninstall-keep-history = Keep history (--keep-history): the release stays listed as uninstalled and can be rolled back
helm-uninstall-delete-claims =
    { $count ->
        [one] Also delete this PVC after the uninstall (its data is lost)
       *[other] Also delete these PVCs after the uninstall (their data is lost)
    }
helm-uninstall-confirm = Uninstall
helm-uninstall-toast = Uninstall of { $name }
helm-uninstall-delete-hooks =
    { $count ->
        [one] The chart's delete hook runs first.
       *[other] The chart's { $count } delete hooks run first.
    }
# Followed by the list of resources in the dialog.
helm-uninstall-resources =
    { $count ->
        [one] Helm deletes release { $name } in { $namespace } and its resource:
       *[other] Helm deletes release { $name } in { $namespace } and its { $count } resources:
    }
helm-uninstall-no-resources = Helm deletes release { $name } in { $namespace }; its manifest has no resources.
helm-claims-stay =
    { $count ->
        [one] This PersistentVolumeClaim stays: Helm does not delete claims created by StatefulSets or kept by the chart, so their volumes and data remain.
       *[other] These { $count } PersistentVolumeClaims stay: Helm does not delete claims created by StatefulSets or kept by the chart, so their volumes and data remain.
    }
helm-claims-check-failed = Can't check which PersistentVolumeClaims stay after the uninstall: { $error }
helm-lookup-interrupted = the lookup was interrupted: { $error }
helm-delete-claim-toast = Delete of PVC { $name }
helm-delete-claims-toast = Delete of { $count } PVCs
helm-delete-claims-detail = In { $namespace }: { $names }
# $reason says how the context changed (helm-context-*).
helm-claims-not-deleted = PVCs not deleted: { $reason }
helm-claims-not-deleted-count =
    { $count ->
        [one] { $count } PVC not deleted
       *[other] { $count } PVCs not deleted
    }
helm-delete-interrupted = deleting was interrupted: { $error }
# Stop of a running install or upgrade; $status is pending-install or
# pending-upgrade.
helm-stop-install-title = Stop the install of { $release }?
helm-stop-upgrade-title = Stop the upgrade of { $release }?
helm-stop-message = Helm is asked to stop waiting and marks the release failed; resources it already created stay, and a running hook Job goes on. If helm does not exit within { $seconds } s it is killed and the release may stay { $status } until it is rolled back or uninstalled.
helm-call-interrupted = helm call was interrupted: { $error }
helm-installed = Installed { $name } in { $namespace }
helm-output-show = Show helm output
helm-output-hide = Hide helm output

## A change asked in one context no longer runs

helm-context-changed = Context changed to { $context }; action cancelled
helm-context-reconnected = Reconnected to { $context }; action cancelled
helm-context-disconnected = Disconnected from { $context }; action cancelled

## Charts page

helm-search-charts = Search charts…
helm-no-home = Can't find Helm's configuration: no home directory.
helm-repositories = Repositories
helm-repo-charts =
    { $count ->
        [one] { $count } chart
       *[other] { $count } charts
    }
helm-repo-index-missing = Not downloaded: update it
helm-repo-index-invalid = Unreadable index: { $error }
helm-repo-show-all = Show the charts of all repositories
helm-repo-show-only = Show only the charts of this repository
helm-repo-update-tooltip = Update the index (helm repo update)
helm-repo-remove-tooltip = Remove the repository (helm repo remove)
helm-repo-update-all-tooltip = Update all indexes (helm repo update)
helm-repo-add-tooltip = Add a repository (helm repo add)
helm-repo-reading = Reading repositories…
helm-repo-none-yet = No repositories yet. Add one by its URL.
helm-repo-name-placeholder = name, e.g. prometheus-community
helm-repo-username-placeholder = username (optional)
helm-repo-password-placeholder = password (optional)
helm-repo-private-note = Private repository: helm stores the login in its repositories file; the password is passed on stdin (--password-stdin). Credentials are only sent to https:// repositories.
helm-repo-add = Add
helm-repo-exists = A repository named { $name } already exists.
helm-repo-username-missing = Enter the username for the password.
helm-repo-config-from = From { $path }
# Progress toast titles of repository commands.
helm-repo-adding = Adding repository { $name }
helm-repo-updating-all = Updating chart repositories
helm-repo-updating = Updating repository { $name }
helm-repo-updating-count = Updating { $count } repositories
helm-repo-removing = Removing repository { $name }
helm-repo-remove-title = Remove repository { $name }?
helm-repo-remove-message = Removes { $name } from your Helm repositories (for helm in the terminal too) and deletes its cached index. Installed releases are not affected.
helm-repo-remove-confirm = Remove
helm-charts-reading = Reading chart repositories…
helm-charts-no-repositories = No chart repositories
helm-charts-no-repositories-description = Add a repository by its URL; Kubach runs helm repo add, so it is your Helm's list, shared with the terminal. Charts from an OCI registry or a URL install without one: Install from OCI or URL….
helm-charts-no-indexes = No indexes downloaded
helm-charts-no-indexes-description = Update the repositories (helm repo update) to search their charts.
helm-charts-repo-no-index = { $name } has no downloaded index: update it to list its charts.
helm-charts-repo-no-match = Nothing in { $name } matches. Clear the repository filter to search all of them.
helm-charts-no-match = Nothing in the cached indexes matches. Update them for new charts.
helm-charts-none-found = No charts found
# $what is charts (one row per chart) or versions (All versions).
helm-charts-summary =
    { $what ->
        [versions]
            { $count ->
                [one] { $count } version in the cached indexes
               *[other] { $count } versions in the cached indexes
            }
       *[charts]
            { $count ->
                [one] { $count } chart in the cached indexes
               *[other] { $count } charts in the cached indexes
            }
    }
helm-charts-summary-cut =
    { $what ->
        [versions] { $count } versions; showing the first { $shown }, refine the search
       *[charts] { $count } charts; showing the first { $shown }, refine the search
    }
helm-charts-repo-chip = repo: { $name }
helm-charts-all-versions = All versions
helm-install-oci = Install from OCI or URL…
helm-install-oci-tooltip = Install an oci:// chart, a path or a URL without a repository
helm-charts-refresh-tooltip = Read the repositories and cached indexes again
helm-chart-deprecated = Deprecated. { $description }
helm-chart-status-deprecated = deprecated

## Chart card

helm-back-charts = Charts
helm-chart-not-found = Chart not found
helm-chart-not-found-description = It is no longer in the repository's cached index.
helm-chart-version = Version { $version }
helm-chart-version-app = { $version }  (app { $app })
helm-install-button = Install…
helm-card-readme = README
helm-card-default-values = Default values
helm-readme-source = Source
helm-readme-source-tooltip = Show the Markdown source
helm-card-needs-helm = Needs the helm CLI
helm-card-needs-helm-description = README and default values come from helm show.
helm-card-load-error = Can't load it
helm-card-no-readme = No README
helm-card-no-values = No default values

## Install panel

helm-install-chart-title = Install a chart
helm-install-chart-placeholder = oci://ghcr.io/org/charts/name, path or URL
helm-install-release-placeholder = release name
helm-install-namespace-placeholder = existing or new namespace
helm-version-latest = latest
# Labels before the fields of a row: "Chart … version …", "Release … namespace …".
helm-label-version = version
helm-label-release = Release
helm-label-namespace = namespace
helm-install-pick-namespace = Pick an existing namespace
helm-install-create-namespace = Create namespace (--create-namespace)
helm-install-namespace-new = New namespace { $namespace }: the install creates it (--create-namespace).
helm-install-namespaces-loading = Loading namespaces to tell whether { $namespace } exists…
helm-install-namespace-checking = Checking whether namespace { $namespace } exists…
helm-install-namespace-unverified-forbidden = Can't check whether namespace { $namespace } exists (no permission to read it): the install creates it if missing (--create-namespace). If it exists and you may not create namespaces, uncheck this.
helm-install-namespace-unverified = Can't check whether namespace { $namespace } exists (the check failed): the install creates it if missing (--create-namespace). If it exists and you may not create namespaces, uncheck this.
helm-values-user = User values. Load the chart defaults to start from them.
helm-values-defaults-loading = Loading the defaults of { $chart }…
helm-values-from-defaults = User values, starting from the chart defaults (helm show values).
helm-values-edited = Values were edited; they started from the defaults of { $chart }.
helm-values-defaults-failed = Can't load the defaults of { $chart }: { $error }
helm-values-defaults-button = Chart defaults
helm-values-defaults-tooltip = Replace the values with the chart defaults
helm-values-defaults-loading-title = Loading the chart defaults…
helm-values-error = Values: { $error }
helm-install-enter-chart = Enter the chart: oci://…, a path or a URL.
helm-install-fix-values = Fix the values: they are not valid YAML.
helm-no-preview = helm did not return a preview
helm-preview-interrupted = preview was interrupted: { $error }
helm-preview = Preview
helm-preview-failed = Preview failed
helm-install-failed = Install failed
helm-install-failed-note = Change the input and install again, or preview it first. If helm got as far as creating the release, it is listed in Releases as failed.
helm-install-preview-title = Preview the install (optional)
helm-install-preview-description = Renders the chart with these values (helm install --dry-run=server) and shows the objects it creates. Nothing is changed in the cluster. You can also install right away.
helm-install-rendering = Rendering the chart…
helm-install-preview-failed-note = Nothing was changed in the cluster. Install stays available without a preview.
helm-install-preview-stale = The input changed since this preview.
helm-install-preview-summary =
    { $count ->
        [one] { $name } in { $namespace } · revision { $revision } · chart { $chart } · { $count } object
       *[other] { $name } in { $namespace } · revision { $revision } · chart { $chart } · { $count } objects
    }
helm-install-no-objects = The chart renders no objects
helm-install-no-objects-description = Only hooks or notes, if any, would run.
helm-install-title = Install { $name }?
# $chart is the chart with its version, if one was picked; $create and
# $previewed are yes or no.
helm-install-confirm-message =
    { $create ->
        [yes]
            { $previewed ->
                [yes] Helm installs { $chart } as release { $release } in namespace { $namespace } (created if missing), as previewed.
               *[no] Helm installs { $chart } as release { $release } in namespace { $namespace } (created if missing), without a preview: the chart was not rendered against the cluster first.
            }
       *[no]
            { $previewed ->
                [yes] Helm installs { $chart } as release { $release } in namespace { $namespace }, as previewed.
               *[no] Helm installs { $chart } as release { $release } in namespace { $namespace }, without a preview: the chart was not rendered against the cluster first.
            }
    }
helm-install-confirm = Install
helm-install-toast = Install of { $name }
helm-missing-crds =
    { $count ->
        [one] The chart creates { $kinds } resources, but the cluster has no CRD for it. Usually the operator that defines it is installed first, as a separate chart. If this chart ships the CRDs itself (its crds/ folder), only the preview fails: a dry run does not create CRDs, so you can install without a preview.
       *[other] The chart creates { $kinds } resources, but the cluster has no CRD for them. Usually the operator that defines them is installed first, as a separate chart. If this chart ships the CRDs itself (its crds/ folder), only the preview fails: a dry run does not create CRDs, so you can install without a preview.
    }

## Upgrade panel

helm-upgrade-chart-placeholder = repo/chart, oci://…, path or URL
helm-version-app = app { $app }
helm-version-prerelease = pre-release
helm-version-current = current
helm-upgrade-stored-loading = Loading the chart kept in the release…
helm-upgrade-stored-error = Stored chart: { $reason }
helm-upgrade-no-repository = No repository has this chart.
helm-upgrade-pick-version = Pick a version of the chart.
helm-upgrade-enter-chart = Enter the chart: repo/chart, oci://…, a path or a URL.
helm-upgrade-source-stored-loading = Chart kept in the release (loading…)
helm-upgrade-source-stored = Chart kept in the release: { $chart }
helm-upgrade-source-stored-unavailable = Chart kept in the release (unavailable)
helm-upgrade-source-repository = Chart from repository { $repository }
helm-upgrade-source-any-repository = Chart from a repository
helm-upgrade-source-reference = Chart from an OCI registry, path or URL
helm-upgrade-stored-unusable = The chart kept in the release can't be used: { $reason }.
helm-upgrade-note-repository = Deployed: { $chart }. Versions come from the cached index of the repository (helm repo update refreshes it); helm downloads the chart.
helm-upgrade-note-reference = Deployed: { $chart }. No repository of yours has this chart; helm resolves the reference with your registry login.
helm-upgrade-preview-title = Preview the upgrade
helm-upgrade-preview-description = Renders the chart with these values on the server (helm upgrade --dry-run=server) and shows how the manifest changes. Nothing is changed in the cluster.
helm-upgrade-rendering = Rendering the upgrade…
helm-upgrade-preview-stale = The input changed since this preview; preview again to upgrade.
helm-upgrade-preview-summary = Revision { $from } → { $to } · chart { $chart }
helm-upgrade-no-change = The manifest does not change
helm-upgrade-no-change-description = Hooks, notes and the stored values may still change.
helm-upgrade-values-note = User values. They replace the release's values on top of the chart defaults (--reset-values).
helm-upgrade-preview-button = Preview changes

## OCI chart versions

helm-oci-listing = Listing the versions in the registry…
helm-oci-list-failed = Can't list the versions: type one
helm-oci-no-versions = The registry has no chart versions here
helm-oci-pick = Pick a version from the registry
helm-oci-list-error = Can't list the chart versions: { $error }. Type the version.

## Form checks

helm-invalid-repository-name-empty = Enter a repository name.
helm-invalid-repository-name-chars = The repository name can't contain '/' or spaces.
helm-invalid-release-empty = Enter a release name.
helm-invalid-release-too-long = The release name is longer than { $max } characters.
helm-invalid-release-chars = The release name may only contain lowercase letters, digits, '-' and '.', and must start and end with a letter or digit.
helm-invalid-namespace-empty = Enter a namespace.
helm-invalid-namespace-chars = The namespace may only contain lowercase letters, digits and '-', at most 63 characters.
helm-invalid-oci-repository = OCI registries are not added as repositories; install an oci:// chart by reference.
helm-invalid-url-scheme = Enter an http:// or https:// URL.
helm-invalid-url = Enter a valid repository URL.
helm-invalid-insecure-credentials = A username and password are only sent over https://. Use an https:// URL or leave the credentials empty.
# $message is the YAML parser's text.
helm-invalid-yaml-line = line { $line }: { $message }
helm-invalid-values-documents = values must be a single YAML document
helm-invalid-values-mapping = line { $line }: values must be a mapping
helm-invalid-no-chart-metadata = the release keeps no chart metadata
helm-invalid-subcharts = the chart has subcharts, which Helm does not keep in the release; upgrade it from its repository or path

## Downloading helm (the hint on the Helm pages)

helm-tools-downloading-helm = Downloading Helm { $version }
helm-tools-download-helm = Download Helm { $version }
# $line is Helm 4 or Helm 3.
helm-tools-download-line = Download { $line }
helm-tools-settings = Settings…
helm-tools-helm-needed = Changes need the helm CLI.
helm-tools-download-tooltip = Official release from get.helm.sh, checked by the Helm maintainers' signature and SHA-256, kept in Kubach's data folder; nothing is installed system-wide
helm-tools-no-data-dir = No data directory to download into.
helm-tools-check-stopped = Check stopped: { $error }
helm-tools-download-stopped = Download stopped: { $error }
helm-tools-remove-failed = Can't remove the downloaded helm: { $error }

## Settings → External tools

helm-tools-auto-downloaded = Auto: { $path } (downloaded)
helm-tools-auto = Auto: { $path }
helm-tools-auto-not-found = Auto: not found on PATH
helm-tools-helm-binary = helm binary
helm-tools-helm-binary-description = Used for Helm operations that need the CLI (ADR 0006). Empty searches PATH, then the copy downloaded by Kubach.
helm-tools-helm-in-use = helm in use
helm-tools-helm-in-use-missing = Not found: upgrade, rollback and uninstall are unavailable; viewing releases works.
# $source is settings (the path set above), path (found on PATH) or
# downloaded (by Kubach).
helm-tools-helm-in-use-description =
    { $source ->
        [settings] { $path } — set above, { $version }
        [path] { $path } — found on PATH, { $version }
       *[downloaded] { $path } — downloaded by Kubach, { $version }
    }
helm-tools-version-unknown = version unknown
helm-tools-helm-line = Helm line to download
helm-tools-helm-line-description = Helm 4 is the current line; Helm 3 gets fixes only. Kubach works with both, also when one is found on PATH.
helm-tools-chart-icons = Load chart icons
helm-tools-chart-icons-description = Downloads chart icons and README images from the URLs the charts give and caches them next to the settings. Off, Kubach sends no such requests (ADR 0006).
helm-tools-downloaded = Downloaded by Kubach
helm-tools-check-updates = Check for updates
helm-tools-remove = Remove
helm-tools-remove-title = Remove the downloaded Helm?
helm-tools-remove-message = Deletes the copy of Helm in Kubach's data folder. Helm actions then use helm from PATH or the path set above, if any; you can download it again here.
helm-tools-signed-by = Signed by { $signer }.
helm-tools-checked-pinned-unsigned = Checked by the SHA-256 pinned in Kubach; signature not verified: { $note }.
helm-tools-checked-pinned = Checked by the SHA-256 pinned in Kubach.
helm-tools-checked-sha = Checked by SHA-256.
helm-tools-checking = Checking for updates…
helm-tools-check-failed = Can't check for updates: { $error }.
# $age is short, like 5m or 3h.
helm-tools-checked-ago = Checked { $age } ago.
helm-tools-latest-unknown = get.helm.sh did not name the latest { $line }.
helm-tools-not-checked = Not checked for updates yet.
helm-tools-unsupported = No Helm download for this OS and CPU: install helm yourself.
helm-tools-no-data-folder = No data folder to download Helm into.
# $checked is one of helm-tools-check*/-checked-ago/-not-checked.
helm-tools-absent = Not downloaded. Available: Helm { $available }. { $checked } Official release from get.helm.sh, checked by the Helm maintainers' OpenPGP signature and SHA-256 and kept in Kubach's data folder; nothing is installed system-wide.
# $verification is one of helm-tools-signed-by/-checked-*.
helm-tools-current = Helm { $version } in { $path }. { $verification } Up to date. { $checked }
helm-tools-outdated = Helm { $version } in { $path }. { $verification } Available: Helm { $available }. { $checked }
helm-tools-update-to = Update to { $version }

## Status bar notices of changes (#459)

helm-notice-install-started = Installing { $name } started
helm-notice-install-done = { $name } installed
helm-notice-install-failed = Install of { $name } failed: { $error }
helm-notice-upgrade-started = Upgrading { $name } started
helm-notice-upgrade-done = { $name } upgraded
helm-notice-upgrade-failed = Upgrade of { $name } failed: { $error }
helm-notice-rollback-started = Rollback of { $name } started
helm-notice-rollback-done = { $name } rolled back
helm-notice-rollback-failed = Rollback of { $name } failed: { $error }
helm-notice-uninstall-started = Uninstalling { $name } started
helm-notice-uninstall-done = { $name } uninstalled
helm-notice-uninstall-failed = Uninstall of { $name } failed: { $error }
