//! Whether `helm install` gets `--create-namespace` (#292).
//!
//! Helm does not create a missing namespace by itself, so an install into a
//! new namespace without the flag fails with `namespaces "…" not found`
//! (its server-side preview does not). Kubach tells a new namespace from
//! an existing one by the watched list of namespaces; while it is still
//! loading the install waits, and when it can't be listed (RBAC, an error)
//! one `GET namespace/<name>` decides. When that GET is refused too, the
//! flag is on by default: helm skips creating a namespace that already
//! exists. It still asks the API server to create it, so a user who may
//! neither read nor create namespaces unchecks it for an existing one.

use k8s_openapi::api::core::v1::Namespace;
use kube::Client;
use kube::api::Api;

/// What is known about the namespaces of the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing<'a, S> {
    /// The list has not loaded yet.
    Loading,
    /// The complete list.
    Listed(&'a [S]),
    /// The namespaces can't be listed (forbidden or failing): the result of
    /// a `GET` of the namespace instead, `None` while it runs.
    Unlisted(Option<Lookup>),
}

/// The result of `GET namespace/<name>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Found,
    NotFound,
    /// Refused (`forbidden`) or failed: existence is unknown.
    Unknown {
        forbidden: bool,
    },
}

/// What the install panel knows about the namespace entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceStatus {
    /// The list or the `GET` is still running.
    Checking,
    /// The namespace exists: `--create-namespace` only if asked for.
    Existing,
    /// The namespace is known not to exist: always created.
    New,
    /// Existence could not be checked: created by default, the user may
    /// opt out.
    Unverified { forbidden: bool },
}

/// Status of the namespace and the `--create-namespace` flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub status: NamespaceStatus,
    /// `--create-namespace`; `None` means the install waits until the
    /// namespace is checked.
    pub create: Option<bool>,
}

impl Decision {
    /// The flag can be changed by the user (not for a new namespace).
    pub fn editable(&self) -> bool {
        self.status != NamespaceStatus::New
    }
}

/// Decides `--create-namespace` for `namespace` (as entered, trimmed). `asked`
/// is the user's checkbox, `None` until it is clicked.
pub fn decide<S: AsRef<str>>(
    namespace: &str,
    listing: Listing<'_, S>,
    asked: Option<bool>,
) -> Decision {
    let status = if namespace.is_empty() {
        // Nothing to check: the form refuses an empty namespace anyway.
        NamespaceStatus::Existing
    } else {
        match listing {
            Listing::Loading | Listing::Unlisted(None) => NamespaceStatus::Checking,
            Listing::Listed(names) => {
                if names.iter().any(|name| name.as_ref() == namespace) {
                    NamespaceStatus::Existing
                } else {
                    NamespaceStatus::New
                }
            }
            Listing::Unlisted(Some(Lookup::Found)) => NamespaceStatus::Existing,
            Listing::Unlisted(Some(Lookup::NotFound)) => NamespaceStatus::New,
            Listing::Unlisted(Some(Lookup::Unknown { forbidden })) => {
                NamespaceStatus::Unverified { forbidden }
            }
        }
    };
    let create = match status {
        // An explicit `--create-namespace` works whether or not the
        // namespace exists: no need to wait for the check.
        NamespaceStatus::Checking => asked.filter(|asked| *asked),
        NamespaceStatus::Existing => Some(asked.unwrap_or(false)),
        NamespaceStatus::New => Some(true),
        NamespaceStatus::Unverified { .. } => Some(asked.unwrap_or(true)),
    };
    Decision { status, create }
}

/// Maps the result of a `GET` of a namespace to a [`Lookup`].
pub fn lookup_result(result: &Result<bool, kube::Error>) -> Lookup {
    match result {
        Ok(true) => Lookup::Found,
        Ok(false) => Lookup::NotFound,
        Err(kube::Error::Api(status)) => Lookup::Unknown {
            forbidden: status.is_forbidden(),
        },
        Err(_) => Lookup::Unknown { forbidden: false },
    }
}

/// `GET namespace/<name>` (metadata only): whether it exists.
pub async fn lookup(client: &Client, name: &str) -> Lookup {
    let api: Api<Namespace> = Api::all(client.clone());
    let result = api
        .get_metadata_opt(name)
        .await
        .map(|found| found.is_some());
    lookup_result(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 2] = ["default", "apps"];

    fn listed() -> Listing<'static, &'static str> {
        Listing::Listed(&NAMES)
    }

    #[test]
    fn listed_existing_namespace_follows_the_checkbox() {
        let decision = decide("apps", listed(), None);
        assert_eq!(decision.status, NamespaceStatus::Existing);
        assert_eq!(decision.create, Some(false));
        assert!(decision.editable());
        assert_eq!(decide("apps", listed(), Some(true)).create, Some(true));
        assert_eq!(decide("apps", listed(), Some(false)).create, Some(false));
    }

    #[test]
    fn listed_missing_namespace_is_always_created() {
        for asked in [None, Some(false), Some(true)] {
            let decision = decide("new-ns", listed(), asked);
            assert_eq!(decision.status, NamespaceStatus::New);
            assert_eq!(decision.create, Some(true));
            assert!(!decision.editable());
        }
    }

    #[test]
    fn an_empty_listed_cluster_has_no_existing_namespace() {
        let none: [&str; 0] = [];
        let decision = decide("apps", Listing::Listed(&none), None);
        assert_eq!(decision.status, NamespaceStatus::New);
        assert_eq!(decision.create, Some(true));
    }

    #[test]
    fn a_loading_list_waits_unless_the_flag_is_asked_for() {
        let loading = Listing::<&str>::Loading;
        let decision = decide("apps", loading, None);
        assert_eq!(decision.status, NamespaceStatus::Checking);
        assert_eq!(decision.create, None);
        assert_eq!(decide("apps", loading, Some(false)).create, None);
        assert_eq!(decide("apps", loading, Some(true)).create, Some(true));
    }

    #[test]
    fn a_running_lookup_waits() {
        let running = Listing::<&str>::Unlisted(None);
        assert_eq!(
            decide("apps", running, None).status,
            NamespaceStatus::Checking
        );
        assert_eq!(decide("apps", running, None).create, None);
        assert_eq!(decide("apps", running, Some(true)).create, Some(true));
    }

    #[test]
    fn unlisted_found_namespace_follows_the_checkbox() {
        let found = Listing::<&str>::Unlisted(Some(Lookup::Found));
        let decision = decide("apps", found, None);
        assert_eq!(decision.status, NamespaceStatus::Existing);
        assert_eq!(decision.create, Some(false));
        assert_eq!(decide("apps", found, Some(true)).create, Some(true));
    }

    #[test]
    fn unlisted_missing_namespace_is_always_created() {
        let missing = Listing::<&str>::Unlisted(Some(Lookup::NotFound));
        for asked in [None, Some(false)] {
            let decision = decide("new-ns", missing, asked);
            assert_eq!(decision.status, NamespaceStatus::New);
            assert_eq!(decision.create, Some(true));
        }
    }

    #[test]
    fn unverified_namespace_is_created_by_default_and_can_be_opted_out() {
        for forbidden in [true, false] {
            let unknown = Listing::<&str>::Unlisted(Some(Lookup::Unknown { forbidden }));
            let decision = decide("apps", unknown, None);
            assert_eq!(decision.status, NamespaceStatus::Unverified { forbidden });
            assert_eq!(decision.create, Some(true));
            assert!(decision.editable());
            assert_eq!(decide("apps", unknown, Some(false)).create, Some(false));
        }
    }

    #[test]
    fn an_empty_namespace_is_not_checked() {
        let decision = decide("", Listing::<&str>::Loading, None);
        assert_eq!(decision.status, NamespaceStatus::Existing);
        assert_eq!(decision.create, Some(false));
    }

    #[test]
    fn lookup_results_map_to_lookups() {
        let api_error = |code: u16, reason: &str| {
            let status = kube::core::Status::failure("refused", reason)
                .with_code(code)
                .boxed();
            Err(kube::Error::Api(status))
        };
        assert_eq!(lookup_result(&Ok(true)), Lookup::Found);
        assert_eq!(lookup_result(&Ok(false)), Lookup::NotFound);
        assert_eq!(
            lookup_result(&api_error(403, "Forbidden")),
            Lookup::Unknown { forbidden: true }
        );
        assert_eq!(
            lookup_result(&api_error(500, "InternalError")),
            Lookup::Unknown { forbidden: false }
        );
    }
}
