//! YAML editing support, UI-free: parse manifests ([`yaml`]), bind them to
//! served kinds ([`resolve`]), produce an editable view of live objects
//! ([`edit`]), validate and complete against the cluster's OpenAPI v3
//! ([`schema`], [`cursor`]), map paths to text ([`locate`]), diff ([`diff`]) and server-side apply
//! ([`server`]).

pub mod cursor;
pub mod diff;
pub mod edit;
pub mod locate;
pub mod path;
pub mod resolve;
pub mod schema;
pub mod server;
pub mod yaml;

pub use cursor::{KeyLine, path_at};
pub use diff::{
    Change, ChangeKind, DiffLine, DiffMode, LineTag, diff_lines, retained_removals, semantic_diff,
    unified_diff, yaml_diff,
};
pub use edit::{edit_view, edit_yaml};
pub use locate::{key_at, kind_at, locate};
pub use path::{FieldPath, PathSegment};
pub use resolve::{ResolveError, ResolvedManifest, resolve, resolve_all};
pub use schema::{Completion, FieldInfo, SchemaCache, SchemaDocument, SchemaError, Violation};
pub use server::{ApplyError, ApplyOptions, ApplyOutcome, FieldConflict, apply, apply_all};
pub use yaml::{Manifest, YamlError, parse, to_yaml, to_yaml_multi};
