# Linked kubeconfig files: the link preview, unlinking, the block about
# programs and files a kubeconfig uses, problems reading a file
# (kubach-core/src/kubeconfig.rs) and "Test connection" failures
# (kubach-core/src/probe.rs). Paths, context, cluster, user and program
# names, environment variables and errors of the OS are data. kubeconfig,
# exec, auth-provider, PATH, RBAC, TLS, CA, VPN, DNS, OIDC stay in English.

## Problems reading a file

kubeconfig-file-missing = File not found
kubeconfig-file-unreadable = Can't read the file: { $error }
kubeconfig-file-invalid = Not a valid kubeconfig: { $error }
kubeconfig-file-locked = The vault is locked

## Unlinking

kubeconfig-unlink-title = Unlink { $name }?
# $contexts is the list of the file's contexts, $count how many there are.
kubeconfig-unlink-description =
    { $count ->
        [0] Kubach stops reading { $path }. It has no contexts in Kubach. The file is not changed; link it again any time.
        [one] Kubach stops reading { $path }. Its context { $contexts } disappears from Kubach. The file is not changed; link it again any time.
       *[other] Kubach stops reading { $path }. All { $count } of its contexts disappear from Kubach: { $contexts }. The file is not changed; link it again any time.
    }
kubeconfig-unlink = Unlink
kubeconfig-restrict-failed = Can't change permissions of { $path }: { $error }

## Link preview

kubeconfig-find-again-title = Find kubeconfig file again
kubeconfig-link-title =
    { $count ->
        [one] Link kubeconfig file
       *[other] Link { $count } kubeconfig files
    }
kubeconfig-link-description = Kubach reads the file in place, so updates by aws, gcloud or other tools are picked up. Credentials are not copied.
kubeconfig-close = Close
kubeconfig-use-file = Use this file
kubeconfig-link-file = Link file
# Button of a preview of several files; $count is how many can be linked.
kubeconfig-link-files =
    { $count ->
        [one] Link { $count } file
       *[other] Link { $count } files
    }
# $mode is like 0644.
kubeconfig-permissions-warning = Other users on this computer can read this file ({ $mode }). It may hold credentials; restrict it to 0600 (chmod 600 { $path }).
kubeconfig-already-linked = This file is already linked.
kubeconfig-no-contexts = The file has no contexts.
kubeconfig-context-shadowed = already defined, this one is skipped
kubeconfig-no-server = No API server
kubeconfig-contexts = Contexts ({ $count })

## What a kubeconfig runs and reads ($purpose: link or import)

kubeconfig-review-programs =
    { $purpose ->
        [link] Connecting runs these programs on this computer. Link only if you expect them.
       *[import] Connecting runs these programs on this computer. Import only if you expect them.
    }
kubeconfig-review-programs-dangerous =
    { $purpose ->
        [link] Connecting runs these programs on this computer. Link only if you expect them. Variables in red can make a program load or start other code.
       *[import] Connecting runs these programs on this computer. Import only if you expect them. Variables in red can make a program load or start other code.
    }
kubeconfig-review-exec = user { $user }, exec
kubeconfig-review-auth-provider = user { $user }, auth-provider { $name }
kubeconfig-review-files-link = Connecting reads these local files; keys and tokens from them are sent to the cluster's server.
kubeconfig-review-files-import = These local files are copied into the vault; keys and tokens from them are sent to the cluster's server.
kubeconfig-review-files-pasted = The pasted config refers to these local files. Kubach reads them into the vault only if you confirm below; keys and tokens from them are sent to the cluster's server.
# Who refers to a file: $owner is a cluster or user name, $key the kubeconfig field.
kubeconfig-review-file-cluster = cluster { $owner }, { $key }
kubeconfig-review-file-user = user { $owner }, { $key }
kubeconfig-review-file-cluster-outside = cluster { $owner }, { $key } (outside the kubeconfig's folder)
kubeconfig-review-file-user-outside = user { $owner }, { $key } (outside the kubeconfig's folder)

## "Test connection" failures

probe-unauthorized = Authentication failed: the server rejected the credentials
probe-forbidden = Signed in, but not allowed to read the server version
probe-unknown-ca = TLS: the server's certificate is not trusted
probe-certificate-name = TLS: the server's certificate is for a different host name
probe-tls = TLS handshake failed
probe-dns = The server's host name doesn't resolve
probe-refused = Connection refused: nothing listens at this address
probe-timeout = The server didn't answer in time
probe-exec-not-found = The exec plugin's command was not found
probe-exec-failed = The exec plugin failed
probe-invalid-config = The connection settings are invalid
probe-server = The server answered with HTTP { $code }
probe-anonymous = Connected, but the server ignored the credentials (anonymous)
probe-other = Can't connect
probe-unauthorized-hint = Check the credentials: they may be expired, revoked or for another cluster. Most clusters (Kubernetes 1.19+) don't accept a user name and password at all.
probe-forbidden-hint = The credentials work, but RBAC denies even /version. Ask the cluster admin for access.
probe-unknown-ca-hint = Use the cluster's CA certificate (certificate-authority-data in its kubeconfig); an expired server certificate fails here too.
probe-certificate-name-hint = Connect by the host name the certificate was issued for, not by IP or alias.
probe-tls-hint = Check the CA certificate, the client certificate and key, and that the URL uses https.
probe-dns-hint = Check the host name, and VPN or DNS settings.
probe-refused-hint = Check the port, and that the cluster is running.
probe-timeout-hint = Check the address, firewalls and VPN; an exec plugin may be waiting for a login.
probe-exec-not-found-hint = Install the plugin (OIDC: kubectl krew install oidc-login), or give its full path; Kubach uses the PATH of your login shell.
probe-exec-failed-hint = Run the plugin command in a terminal to see why it fails.
probe-anonymous-hint = The server doesn't support this way of signing in: Kubernetes 1.19+ ignores a user name and password. Use a token, client certificate, OIDC or exec plugin.
