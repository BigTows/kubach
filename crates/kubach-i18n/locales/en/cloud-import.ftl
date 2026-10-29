# "Import from cloud" (#38): clusters of Google GKE, Yandex Cloud and AWS EKS
# listed through the provider's CLI, and the cloud CLIs in Settings → Tools.
vault-add-cloud = Import from cloud…
vault-add-cloud-text = Google GKE, Yandex Cloud or AWS EKS: pick clusters the provider's CLI can see. Kubach keeps no cloud tokens; the contexts sign in through the CLI. Stored in the encrypted vault.
cloud-title = Import from cloud
cloud-refresh = Refresh
# $tool is a product name such as "AWS CLI"; $url is the vendor's install page.
cloud-cli-cannot-download = Kubach can't download { $tool } for this system. Install it from { $url }, then press Refresh.
cloud-cli-missing = { $tool } was not found in PATH.
cloud-cli-download = Download { $tool }
cloud-cli-download-tooltip = The vendor's official release, checked wherever the vendor publishes a checksum or signature, into Kubach's data folder. Nothing is installed system-wide.
cloud-cli-in-path = Using { $path } (from PATH)
cloud-cli-downloaded = Using { $path } (downloaded by Kubach)
cloud-gke-plugin-missing = gke-gcloud-auth-plugin is not installed; GKE contexts need it to sign in. Install it with: gcloud components install gke-gcloud-auth-plugin
cloud-gke-plugin-install = Install the plugin
cloud-gke-plugin-installing = Installing the plugin…
cloud-gke-plugin-failed = gke-gcloud-auth-plugin was not installed: { $error }
cloud-gke-python-hint = On macOS gcloud needs Python 3.10 or newer: install it (python.org or brew install python) or point CLOUDSDK_PYTHON at it, then try again.
# $tool is the command: gcloud, yc or aws.
cloud-not-logged-in = { $tool } is not signed in. Sign in in a terminal, then press Try again:
cloud-error = { $tool } failed: { $error }
cloud-try-again = Try again
cloud-loading = Asking { $tool }…
cloud-scopes-gke = Projects
cloud-scopes-yandex = Folders
cloud-scopes-eks = Regions
cloud-profile = Profile
cloud-account = Signed in as { $account }
cloud-no-scopes = Nothing found for this account.
cloud-find-clusters = Find clusters
cloud-finding = Looking for clusters…
cloud-endpoint-external = External
cloud-endpoint-internal = Internal
cloud-endpoint-dns = DNS
cloud-clusters =
    { $count ->
        [one] { $count } cluster
       *[other] { $count } clusters
    }
cloud-no-clusters = No clusters in the chosen places.
cloud-scope-failed = { $scope }: { $error }
cloud-note = Contexts run { $tool } to sign in each time Kubach connects; Kubach does not store cloud tokens.
cloud-selected =
    { $count ->
        [one] { $count } cluster selected
       *[other] { $count } clusters selected
    }
cloud-next = Next
cloud-none-selected = Check at least one cluster.
cloud-tools-group = Cloud CLIs
cloud-tools-downloading = Downloading { $tool }…
cloud-tools-in-path = In use: { $path } from PATH, { $version }.
cloud-tools-in-use-downloaded = In use: { $path }, downloaded by Kubach, { $version }.
cloud-tools-missing = Not found. Import from cloud needs it.
# $verification is one of cloud-tools-checked-*/-signed/-unchecked or helm-tools-signed-by.
cloud-tools-downloaded = Downloaded copy: { $version }. { $verification }
cloud-tools-unsupported = Kubach can't download { $tool } for this system: install it from { $url }.
cloud-tools-download = Download
cloud-tools-update = Update
cloud-tools-remove-title = Remove the downloaded { $tool }?
cloud-tools-remove-message = Deletes the copy in Kubach's data folder. Contexts imported with it can't sign in until { $tool } is installed or downloaded again.
cloud-tools-checked-published = Checked by the SHA-256 published at { $source }.
cloud-tools-package-signed = Package signed by { $signer }.
cloud-tools-unchecked = Not checked: the vendor publishes no checksum or signature; downloaded over HTTPS from the vendor's server.
