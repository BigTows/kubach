//! Typed validation errors of form fields and user-supplied content (Helm
//! repositories, releases and values, new Secrets). The UI shows them via
//! [`Display`](std::fmt::Display); the texts are the ones the forms always
//! showed. [`ValidationError::field`] says which field is at fault.

use std::fmt;

use crate::names::NameError;
use crate::secrets::x509::CertificateError;

/// The field or piece of content a [`ValidationError`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// Helm repository name (`helm repo add NAME`).
    RepositoryName,
    /// Helm repository URL.
    RepositoryUrl,
    /// Helm repository username and password.
    RepositoryCredentials,
    /// Helm release name.
    ReleaseName,
    /// Namespace name.
    Namespace,
    /// Kubernetes object name (DNS-1123 subdomain), e.g. a new Secret's.
    Name,
    /// Helm values YAML.
    Values,
    /// A Secret data key.
    SecretKey,
    /// Registry server of a docker-registry Secret.
    RegistryServer,
    /// User name of a docker-registry Secret.
    Username,
    /// Password of a docker-registry Secret.
    Password,
    /// `.dockerconfigjson` / `.dockercfg` content.
    DockerConfig,
    /// `tls.crt` content.
    TlsCert,
    /// `tls.key` content.
    TlsKey,
    /// A Helm chart kept in a release.
    StoredChart,
}

impl Field {
    /// Lowercase name of the field, for messages without a dedicated text.
    pub fn label(self) -> &'static str {
        match self {
            Self::RepositoryName => "repository name",
            Self::RepositoryUrl => "repository URL",
            Self::RepositoryCredentials => "credentials",
            Self::ReleaseName => "release name",
            Self::Namespace => "namespace",
            Self::Name => "name",
            Self::Values => "values",
            Self::SecretKey => "key",
            Self::RegistryServer => "registry server",
            Self::Username => "user name",
            Self::Password => "password",
            Self::DockerConfig => "docker config",
            Self::TlsCert => "tls.crt",
            Self::TlsKey => "key file",
            Self::StoredChart => "chart",
        }
    }
}

/// Why a field or a piece of content is not valid.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// Nothing entered.
    #[error("{}", empty_message(*.0))]
    Empty(Field),
    /// Longer than allowed; `value` is the offending text (never a secret).
    #[error("{}", too_long_message(*.field, .value, *.max))]
    TooLong {
        field: Field,
        value: String,
        max: usize,
    },
    /// Holds characters the field does not allow.
    #[error("{}", invalid_chars_message(*.field, .value))]
    InvalidChars { field: Field, value: String },
    /// A value the field reserves (the keys `.` and `..`).
    #[error("Key {value} is not allowed")]
    Reserved { field: Field, value: String },
    /// The same value entered twice.
    #[error("Key {value} is used twice")]
    Duplicate { field: Field, value: String },
    /// Not a valid Kubernetes name (DNS-1123).
    #[error("{}", name_message(*.field, .error))]
    Name { field: Field, error: NameError },
    /// An OCI registry where a chart repository is expected.
    #[error("OCI registries are not added as repositories; install an oci:// chart by reference.")]
    OciRepository,
    /// Not an `http://` / `https://` URL.
    #[error("Enter an http:// or https:// URL.")]
    UrlScheme,
    /// A URL with nothing usable after the scheme.
    #[error("Enter a valid repository URL.")]
    InvalidUrl,
    /// Credentials for a repository not reached over `https://` (#202).
    #[error(
        "A username and password are only sent over https://. \
         Use an https:// URL or leave the credentials empty."
    )]
    InsecureCredentials,
    /// Invalid YAML; `line` when the parser knows it.
    #[error("{}", yaml_message(*.line, .message))]
    Yaml {
        field: Option<Field>,
        line: Option<usize>,
        message: String,
    },
    /// More than one YAML document where one is expected.
    #[error("values must be a single YAML document")]
    MultipleDocuments(Field),
    /// A YAML document that is not a mapping.
    #[error("line {line}: values must be a mapping")]
    NotMapping { field: Field, line: usize },
    /// Not valid JSON.
    #[error("The {} is not valid JSON", .0.label())]
    Json(Field),
    /// Not PEM text.
    #[error("{} is not PEM text", .0.label())]
    Pem(Field),
    /// Readable, but holds nothing of what is expected.
    #[error("{}", nothing_message(*.0))]
    Nothing(Field),
    /// A PEM certificate block that is not a valid certificate.
    #[error(transparent)]
    Certificate(#[from] CertificateError),
    /// The Secret lacks a data key its type needs.
    #[error("The Secret has no {0} key")]
    MissingKey(String),
    /// The release keeps no `Chart.yaml` metadata.
    #[error("the release keeps no chart metadata")]
    NoChartMetadata,
    /// The stored chart has subcharts, which Helm does not keep.
    #[error(
        "the chart has subcharts, which Helm does not keep in the release; \
         upgrade it from its repository or path"
    )]
    Subcharts,
}

impl ValidationError {
    /// The field at fault, when the error is about one.
    pub fn field(&self) -> Option<Field> {
        match self {
            Self::Empty(field)
            | Self::MultipleDocuments(field)
            | Self::Json(field)
            | Self::Pem(field)
            | Self::Nothing(field)
            | Self::TooLong { field, .. }
            | Self::InvalidChars { field, .. }
            | Self::Reserved { field, .. }
            | Self::Duplicate { field, .. }
            | Self::Name { field, .. }
            | Self::NotMapping { field, .. } => Some(*field),
            Self::Yaml { field, .. } => *field,
            Self::OciRepository | Self::UrlScheme | Self::InvalidUrl => Some(Field::RepositoryUrl),
            Self::InsecureCredentials => Some(Field::RepositoryCredentials),
            Self::Certificate(_) => Some(Field::TlsCert),
            Self::NoChartMetadata | Self::Subcharts => Some(Field::StoredChart),
            Self::MissingKey(_) => None,
        }
    }
}

/// A plain object name ([`Field::Name`]).
impl From<NameError> for ValidationError {
    fn from(error: NameError) -> Self {
        Self::Name {
            field: Field::Name,
            error,
        }
    }
}

/// For callers that still pass errors around as text.
impl From<ValidationError> for String {
    fn from(error: ValidationError) -> Self {
        error.to_string()
    }
}

fn empty_message(field: Field) -> String {
    match field {
        Field::RepositoryName => "Enter a repository name.".into(),
        Field::ReleaseName => "Enter a release name.".into(),
        Field::Namespace => "Enter a namespace.".into(),
        Field::SecretKey => "A key can't be empty".into(),
        Field::RegistryServer => "Enter the registry server".into(),
        Field::Username => "Enter the user name".into(),
        Field::Password => "Enter the password".into(),
        other => format!("Enter the {}", other.label()),
    }
}

fn too_long_message(field: Field, value: &str, max: usize) -> String {
    match field {
        Field::SecretKey => format!("Key {value} is longer than {max} characters"),
        other => format!("The {} is longer than {max} characters", other.label()),
    }
}

fn invalid_chars_message(field: Field, value: &str) -> String {
    match field {
        Field::RepositoryName => "The repository name can't contain '/' or spaces.".into(),
        Field::SecretKey => {
            format!("Key {value} may only contain letters, digits, '-', '_' and '.'")
        }
        other => format!("The {} contains invalid characters", other.label()),
    }
}

fn name_message(field: Field, error: &NameError) -> String {
    match (field, error) {
        (Field::ReleaseName, NameError::Empty) | (Field::Namespace, NameError::Empty) => {
            empty_message(field)
        }
        (Field::ReleaseName, NameError::TooLong { .. }) => format!(
            "The release name is longer than {} characters.",
            crate::helm::cli::MAX_RELEASE_NAME
        ),
        (Field::ReleaseName, _) => "The release name may only contain lowercase letters, \
             digits, '-' and '.', and must start and end with a letter or digit."
            .into(),
        (Field::Namespace, _) => "The namespace may only contain lowercase letters, digits \
             and '-', at most 63 characters."
            .into(),
        _ => "The name must be lowercase letters, digits, '-' or '.', starting and ending \
             with a letter or digit"
            .into(),
    }
}

fn yaml_message(line: Option<usize>, message: &str) -> String {
    match line {
        Some(line) => format!("line {line}: {message}"),
        None => message.to_owned(),
    }
}

fn nothing_message(field: Field) -> String {
    match field {
        Field::DockerConfig => "The docker config has no registries".into(),
        Field::TlsCert => "tls.crt holds no PEM certificate".into(),
        Field::TlsKey => "The key file holds no PEM private key".into(),
        other => format!("The {} is empty", other.label()),
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::{Field, ValidationError};
    use crate::names::NameError;

    fn text(error: ValidationError) -> String {
        String::from(error)
    }

    /// Each variant shows the text the forms showed before it existed.
    #[test]
    fn messages_match_the_old_texts() {
        use ValidationError::*;
        let cases: Vec<(ValidationError, &str)> = vec![
            (Empty(Field::RepositoryName), "Enter a repository name."),
            (Empty(Field::SecretKey), "A key can't be empty"),
            (Empty(Field::RegistryServer), "Enter the registry server"),
            (Empty(Field::Username), "Enter the user name"),
            (Empty(Field::Password), "Enter the password"),
            (
                TooLong {
                    field: Field::SecretKey,
                    value: "k".into(),
                    max: 253,
                },
                "Key k is longer than 253 characters",
            ),
            (
                InvalidChars {
                    field: Field::RepositoryName,
                    value: "a/b".into(),
                },
                "The repository name can't contain '/' or spaces.",
            ),
            (
                InvalidChars {
                    field: Field::SecretKey,
                    value: "a/b".into(),
                },
                "Key a/b may only contain letters, digits, '-', '_' and '.'",
            ),
            (
                Reserved {
                    field: Field::SecretKey,
                    value: "..".into(),
                },
                "Key .. is not allowed",
            ),
            (
                Duplicate {
                    field: Field::SecretKey,
                    value: "a".into(),
                },
                "Key a is used twice",
            ),
            (
                Name {
                    field: Field::ReleaseName,
                    error: NameError::Empty,
                },
                "Enter a release name.",
            ),
            (
                Name {
                    field: Field::ReleaseName,
                    error: NameError::TooLong { max: 53 },
                },
                "The release name is longer than 53 characters.",
            ),
            (
                Name {
                    field: Field::ReleaseName,
                    error: NameError::BadEdge,
                },
                "The release name may only contain lowercase letters, digits, '-' and '.', \
                 and must start and end with a letter or digit.",
            ),
            (
                Name {
                    field: Field::Namespace,
                    error: NameError::Empty,
                },
                "Enter a namespace.",
            ),
            (
                Name {
                    field: Field::Namespace,
                    error: NameError::InvalidChar('.'),
                },
                "The namespace may only contain lowercase letters, digits and '-', \
                 at most 63 characters.",
            ),
            (
                NameError::EmptyLabel.into(),
                "The name must be lowercase letters, digits, '-' or '.', starting and ending \
                 with a letter or digit",
            ),
            (
                OciRepository,
                "OCI registries are not added as repositories; install an oci:// chart by \
                 reference.",
            ),
            (UrlScheme, "Enter an http:// or https:// URL."),
            (InvalidUrl, "Enter a valid repository URL."),
            (
                InsecureCredentials,
                "A username and password are only sent over https://. \
                 Use an https:// URL or leave the credentials empty.",
            ),
            (
                Yaml {
                    field: Some(Field::Values),
                    line: Some(2),
                    message: "bad".into(),
                },
                "line 2: bad",
            ),
            (
                Yaml {
                    field: None,
                    line: None,
                    message: "bad".into(),
                },
                "bad",
            ),
            (
                MultipleDocuments(Field::Values),
                "values must be a single YAML document",
            ),
            (
                NotMapping {
                    field: Field::Values,
                    line: 1,
                },
                "line 1: values must be a mapping",
            ),
            (
                Json(Field::DockerConfig),
                "The docker config is not valid JSON",
            ),
            (Pem(Field::TlsCert), "tls.crt is not PEM text"),
            (
                Nothing(Field::DockerConfig),
                "The docker config has no registries",
            ),
            (Nothing(Field::TlsCert), "tls.crt holds no PEM certificate"),
            (
                Nothing(Field::TlsKey),
                "The key file holds no PEM private key",
            ),
            (
                MissingKey("tls.crt".into()),
                "The Secret has no tls.crt key",
            ),
            (NoChartMetadata, "the release keeps no chart metadata"),
            (
                Subcharts,
                "the chart has subcharts, which Helm does not keep in the release; \
                 upgrade it from its repository or path",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(text(error), expected);
        }
    }

    #[test]
    fn fields() {
        assert_eq!(
            ValidationError::from(NameError::Empty).field(),
            Some(Field::Name)
        );
        assert_eq!(
            ValidationError::UrlScheme.field(),
            Some(Field::RepositoryUrl)
        );
        assert_eq!(
            ValidationError::InsecureCredentials.field(),
            Some(Field::RepositoryCredentials)
        );
        assert_eq!(
            ValidationError::Empty(Field::Password).field(),
            Some(Field::Password)
        );
        assert_eq!(ValidationError::MissingKey("x".into()).field(), None);
    }
}
