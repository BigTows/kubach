//! Server-side apply with dry-run preview and readable conflicts.

use kube::Client;
use kube::api::{DynamicObject, Patch, PatchParams};
use kube::core::response::StatusCause;
use serde_json::Value;

use super::diff::{Change, DiffMode, semantic_diff, yaml_diff};
use super::resolve::ResolvedManifest;
use crate::discovery;

/// Field manager Kubach applies as.
pub const FIELD_MANAGER: &str = "kubach";

#[derive(Clone, Debug)]
pub struct ApplyOptions {
    pub field_manager: String,
    /// Take ownership of fields other managers own instead of failing.
    pub force: bool,
    /// Let the server validate, default and admit, without persisting.
    pub dry_run: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            field_manager: FIELD_MANAGER.into(),
            force: false,
            dry_run: false,
        }
    }
}

impl ApplyOptions {
    pub fn dry_run() -> Self {
        Self {
            dry_run: true,
            ..Self::default()
        }
    }

    fn params(&self) -> PatchParams {
        let mut params = PatchParams::apply(&self.field_manager);
        if self.force {
            params = params.force();
        }
        if self.dry_run {
            params = params.dry_run();
        }
        params
    }
}

/// What an apply did (or would do, for a dry run).
#[derive(Clone, Debug)]
pub struct ApplyOutcome {
    /// The live object before; `None` if it is being created.
    pub before: Option<DynamicObject>,
    /// The object as the server returned it (defaulted, admitted).
    pub after: DynamicObject,
    pub dry_run: bool,
    /// Field changes from `before` to `after`.
    pub changes: Vec<Change>,
    /// Unified YAML diff from `before` to `after`.
    pub diff: String,
}

impl ApplyOutcome {
    pub fn created(&self) -> bool {
        self.before.is_none()
    }
}

/// A field another manager owns with a different value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldConflict {
    /// `.spec.replicas` style.
    pub field: String,
    pub manager: String,
}

/// A field the server rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidField {
    pub field: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("failed to read {label}: {source}")]
    Get {
        label: String,
        #[source]
        source: Box<kube::Error>,
    },
    #[error("{}", conflicts_message(.label, .conflicts))]
    Conflicts {
        label: String,
        conflicts: Vec<FieldConflict>,
    },
    #[error(
        "{label} was changed since it was loaded (resourceVersion mismatch); reload it and apply again"
    )]
    Stale { label: String },
    #[error("{label} is invalid: {message}")]
    Invalid {
        label: String,
        message: String,
        fields: Vec<InvalidField>,
    },
    #[error("failed to apply {label}: {source}")]
    Api {
        label: String,
        #[source]
        source: Box<kube::Error>,
    },
}

fn conflicts_message(label: &str, conflicts: &[FieldConflict]) -> String {
    let mut message = format!(
        "{label}: {} field(s) are owned by other managers; force the apply to take them over:",
        conflicts.len()
    );
    for conflict in conflicts {
        message.push_str(&format!(
            "\n  {} (owned by \"{}\")",
            conflict.field, conflict.manager
        ));
    }
    message
}

/// Applies one manifest. Its `metadata.resourceVersion`, if any, makes the
/// apply fail with [`ApplyError::Stale`] when the object changed meanwhile.
pub async fn apply(
    client: &Client,
    manifest: &ResolvedManifest,
    options: &ApplyOptions,
) -> Result<ApplyOutcome, ApplyError> {
    let label = manifest.label();
    let api = discovery::api(client, &manifest.kind, manifest.namespace.as_deref());
    let before = api
        .get_opt(&manifest.name)
        .await
        .map_err(|source| ApplyError::Get {
            label: label.clone(),
            source: Box::new(source),
        })?;

    let mut body = manifest.value.clone();
    if let Some(metadata) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        // The server rejects apply configurations that carry managedFields.
        metadata.remove("managedFields");
    }
    let after = api
        .patch(&manifest.name, &options.params(), &Patch::Apply(&body))
        .await
        .map_err(|error| map_error(label, error))?;

    let before_value = before.as_ref().map(to_value);
    let after_value = to_value(&after);
    let changes = match &before_value {
        Some(before) => semantic_diff(before, &after_value, DiffMode::Full),
        None => semantic_diff(
            &Value::Object(Default::default()),
            &after_value,
            DiffMode::Full,
        ),
    };
    Ok(ApplyOutcome {
        diff: yaml_diff(before_value.as_ref(), &after_value),
        before,
        after,
        dry_run: options.dry_run,
        changes,
    })
}

/// Applies manifests in order, stopping at nothing: every result is
/// returned so the caller can show each one.
pub async fn apply_all(
    client: &Client,
    manifests: &[ResolvedManifest],
    options: &ApplyOptions,
) -> Vec<Result<ApplyOutcome, ApplyError>> {
    let mut results = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        results.push(apply(client, manifest, options).await);
    }
    results
}

fn to_value(object: &DynamicObject) -> Value {
    serde_json::to_value(object).unwrap_or(Value::Null)
}

fn map_error(label: String, error: kube::Error) -> ApplyError {
    let kube::Error::Api(status) = &error else {
        return ApplyError::Api {
            label,
            source: Box::new(error),
        };
    };
    let causes = status
        .details
        .as_ref()
        .map(|details| details.causes.as_slice())
        .unwrap_or_default();
    if status.is_conflict() {
        let conflicts = field_conflicts(causes, &status.message);
        return if conflicts.is_empty() {
            ApplyError::Stale { label }
        } else {
            ApplyError::Conflicts { label, conflicts }
        };
    }
    if status.is_invalid() {
        return ApplyError::Invalid {
            label,
            message: status.message.clone(),
            fields: causes
                .iter()
                .map(|cause| InvalidField {
                    field: cause.field.clone(),
                    message: cause.message.clone(),
                })
                .collect(),
        };
    }
    ApplyError::Api {
        label,
        source: Box::new(error),
    }
}

/// Field-manager conflicts from a 409's causes, or from its message when
/// the server sent no causes.
fn field_conflicts(causes: &[StatusCause], message: &str) -> Vec<FieldConflict> {
    let from_causes: Vec<_> = causes
        .iter()
        .filter(|cause| cause.reason == "FieldManagerConflict")
        .map(|cause| FieldConflict {
            field: cause.field.clone(),
            manager: quoted(&cause.message).unwrap_or_default(),
        })
        .collect();
    if !from_causes.is_empty() {
        return from_causes;
    }
    // `Apply failed with 2 conflicts: conflicts with "m" using v1:\n- .a\n- .b`
    // or, for one conflict, `... conflict with "m" using v1: .a`.
    let mut conflicts = Vec::new();
    let mut manager = None;
    for line in message.lines() {
        if let Some(found) = line
            .find("conflict with")
            .or_else(|| line.find("conflicts with"))
        {
            manager = quoted(&line[found..]);
            if let Some((_, field)) = line[found..].split_once(": ")
                && field.starts_with('.')
            {
                conflicts.push(FieldConflict {
                    field: field.trim().to_string(),
                    manager: manager.clone().unwrap_or_default(),
                });
            }
        } else if let Some(field) = line.trim().strip_prefix("- ")
            && let Some(manager) = &manager
        {
            conflicts.push(FieldConflict {
                field: field.to_string(),
                manager: manager.clone(),
            });
        }
    }
    conflicts
}

fn quoted(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let end = start + text[start..].find('"')?;
    Some(text[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::response::{Status, StatusDetails};

    fn cause(manager: &str, field: &str) -> StatusCause {
        StatusCause {
            reason: "FieldManagerConflict".into(),
            message: format!("conflict with \"{manager}\" using v1"),
            field: field.into(),
        }
    }

    fn api_error(code: u16, reason: &str, message: &str, causes: Vec<StatusCause>) -> kube::Error {
        let details = StatusDetails {
            name: "x".into(),
            group: String::new(),
            kind: "configmaps".into(),
            uid: String::new(),
            causes,
            retry_after_seconds: 0,
        };
        kube::Error::Api(
            Status::failure(message, reason)
                .with_code(code)
                .with_details(details)
                .boxed(),
        )
    }

    #[test]
    fn maps_field_manager_conflicts() {
        let error = map_error(
            "ConfigMap web/x".into(),
            api_error(
                409,
                "Conflict",
                "Apply failed with 2 conflicts",
                vec![cause("kubectl", ".data.a"), cause("helm", ".data.b")],
            ),
        );
        let ApplyError::Conflicts { conflicts, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(conflicts[1].manager, "helm");
        assert_eq!(
            error.to_string(),
            "ConfigMap web/x: 2 field(s) are owned by other managers; force the apply to take them over:\
             \n  .data.a (owned by \"kubectl\")\n  .data.b (owned by \"helm\")"
        );
    }

    #[test]
    fn parses_conflicts_from_message() {
        let many = "Apply failed with 2 conflicts: conflicts with \"kubectl-edit\" using apps/v1:\n- .spec.replicas\n- .spec.paused";
        assert_eq!(
            field_conflicts(&[], many),
            [
                FieldConflict {
                    field: ".spec.replicas".into(),
                    manager: "kubectl-edit".into()
                },
                FieldConflict {
                    field: ".spec.paused".into(),
                    manager: "kubectl-edit".into()
                },
            ]
        );
        let one = "Apply failed with 1 conflict: conflict with \"m\" using v1: .data.key";
        assert_eq!(
            field_conflicts(&[], one),
            [FieldConflict {
                field: ".data.key".into(),
                manager: "m".into()
            }]
        );
    }

    #[test]
    fn maps_stale_and_invalid() {
        let stale = map_error(
            "ConfigMap web/x".into(),
            api_error(409, "Conflict", "the object has been modified", Vec::new()),
        );
        assert!(matches!(stale, ApplyError::Stale { .. }), "{stale:?}");

        let invalid = map_error(
            "ConfigMap web/x".into(),
            api_error(
                422,
                "Invalid",
                "ConfigMap \"x\" is invalid",
                vec![StatusCause {
                    reason: "FieldValueInvalid".into(),
                    message: "Invalid value".into(),
                    field: "data[a b]".into(),
                }],
            ),
        );
        let ApplyError::Invalid { fields, .. } = invalid else {
            panic!();
        };
        assert_eq!(fields[0].field, "data[a b]");
    }

    #[test]
    fn builds_params() {
        let params = ApplyOptions {
            force: true,
            ..ApplyOptions::dry_run()
        }
        .params();
        assert!(params.force && params.dry_run);
        assert_eq!(params.field_manager.as_deref(), Some(FIELD_MANAGER));
    }
}
