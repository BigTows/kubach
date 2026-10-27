//! The chart a release keeps in its storage object, written back to disk so
//! `helm upgrade` can render the same chart with new values (ADR 0006).
//!
//! Helm stores `Chart.yaml`, default values, the values schema, templates and
//! other files, but not subcharts: a chart with dependencies can't be rebuilt
//! from the release and has to be upgraded from its repository or path.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::Value;

use super::{HelmError, values_to_yaml};
use crate::validation::ValidationError;

/// A template or other file of a chart.
#[derive(Clone, PartialEq, Eq)]
pub struct ChartFile {
    /// Path inside the chart (`templates/deployment.yaml`).
    pub name: String,
    pub data: Vec<u8>,
}

/// A chart as kept in a release. Default values may hold secrets, so
/// `Debug` shows only names.
#[derive(Clone, PartialEq)]
pub struct StoredChart {
    metadata: Value,
    values: Value,
    schema: Option<Vec<u8>>,
    templates: Vec<ChartFile>,
    files: Vec<ChartFile>,
}

#[derive(Deserialize)]
struct RawChart {
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    values: Option<Value>,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    templates: Option<Vec<RawFile>>,
    #[serde(default)]
    files: Option<Vec<RawFile>>,
}

/// Go serializes `[]byte` as base64.
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    data: Option<String>,
}

impl RawFile {
    fn decode(self) -> Result<ChartFile, HelmError> {
        Ok(ChartFile {
            name: self.name,
            data: STANDARD.decode(self.data.unwrap_or_default())?,
        })
    }
}

impl StoredChart {
    /// Parses the `chart` object of release JSON.
    pub fn from_json(chart: Value) -> Result<Self, HelmError> {
        let raw: RawChart = serde_json::from_value(chart)?;
        let decode_all = |files: Option<Vec<RawFile>>| {
            files
                .unwrap_or_default()
                .into_iter()
                .map(RawFile::decode)
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            metadata: raw.metadata.unwrap_or(Value::Null),
            values: raw.values.unwrap_or(Value::Null),
            schema: raw
                .schema
                .filter(|schema| !schema.is_empty())
                .map(|schema| STANDARD.decode(schema))
                .transpose()?,
            templates: decode_all(raw.templates)?,
            files: decode_all(raw.files)?,
        })
    }

    /// `Chart.yaml` as JSON.
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    pub fn templates(&self) -> &[ChartFile] {
        &self.templates
    }

    /// `name-version` from `Chart.yaml`.
    pub fn reference(&self) -> String {
        let field = |key: &str| self.metadata[key].as_str().unwrap_or_default().to_owned();
        format!("{}-{}", field("name"), field("version"))
    }

    /// The chart declares subcharts, which the release does not keep.
    pub fn has_dependencies(&self) -> bool {
        self.metadata["dependencies"]
            .as_array()
            .is_some_and(|deps| !deps.is_empty())
    }

    /// Whether `helm upgrade` can use this chart: it has metadata and no
    /// subcharts.
    pub fn usable(&self) -> Result<(), ValidationError> {
        if !self.metadata.is_object() || self.metadata["name"].as_str().is_none() {
            return Err(ValidationError::NoChartMetadata);
        }
        if self.has_dependencies() {
            return Err(ValidationError::Subcharts);
        }
        Ok(())
    }

    /// Writes the chart into a new `chart` directory under `dir` and returns
    /// its path. File names that would leave the chart directory are refused.
    pub fn write_to(&self, dir: &Path) -> io::Result<PathBuf> {
        let root = dir.join("chart");
        std::fs::create_dir(&root)?;
        for file in self.files.iter().chain(&self.templates) {
            let relative = safe_relative(&file.name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsafe file name in the chart: {}", file.name),
                )
            })?;
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, &file.data)?;
        }
        // Written last: a stray file of the same name can't replace them.
        let chart_yaml = serde_saphyr::to_string(&self.metadata)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        std::fs::write(root.join("Chart.yaml"), chart_yaml)?;
        let values = match &self.values {
            Value::Null => "{}\n".to_owned(),
            values => values_to_yaml(values),
        };
        std::fs::write(root.join("values.yaml"), values)?;
        if let Some(schema) = &self.schema {
            std::fs::write(root.join("values.schema.json"), schema)?;
        }
        Ok(root)
    }
}

impl fmt::Debug for StoredChart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredChart")
            .field("chart", &self.reference())
            .field(
                "templates",
                &self.templates.iter().map(|t| &t.name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ChartFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChartFile")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A relative path with only normal components (`a/b.yaml`).
fn safe_relative(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let normal = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (normal && path.components().next().is_some()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chart(dependencies: Value) -> StoredChart {
        StoredChart::from_json(json!({
            "metadata": {"apiVersion": "v2", "name": "web", "version": "1.0.0",
                         "dependencies": dependencies},
            "values": {"replicaCount": 1, "auth": {"password": "hunter2"}},
            "schema": STANDARD.encode(r#"{"type":"object"}"#),
            "templates": [
                {"name": "templates/deployment.yaml", "data": STANDARD.encode("kind: Deployment\n")},
                {"name": "templates/tests/test.yaml", "data": STANDARD.encode("kind: Pod\n")}
            ],
            "files": [{"name": ".helmignore", "data": STANDARD.encode("*.tmp\n")}]
        }))
        .unwrap()
    }

    #[test]
    fn writes_a_loadable_chart_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = chart(Value::Null).write_to(dir.path()).unwrap();
        let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
        assert!(read("Chart.yaml").contains("name: web"));
        assert!(read("Chart.yaml").starts_with("apiVersion: v2"));
        assert_eq!(
            read("values.yaml"),
            "auth:\n  password: hunter2\nreplicaCount: 1\n"
        );
        assert_eq!(read("values.schema.json"), r#"{"type":"object"}"#);
        assert_eq!(read("templates/deployment.yaml"), "kind: Deployment\n");
        assert_eq!(read("templates/tests/test.yaml"), "kind: Pod\n");
        assert_eq!(read(".helmignore"), "*.tmp\n");
    }

    #[test]
    fn charts_with_subcharts_are_not_usable() {
        assert!(chart(Value::Null).usable().is_ok());
        assert!(chart(json!([])).usable().is_ok());
        let with_deps = chart(json!([{"name": "redis", "version": "1.x"}]));
        assert!(with_deps.has_dependencies());
        assert_eq!(with_deps.usable(), Err(ValidationError::Subcharts));
        let empty = StoredChart::from_json(json!({})).unwrap();
        assert_eq!(empty.usable(), Err(ValidationError::NoChartMetadata));
    }

    #[test]
    fn refuses_file_names_outside_the_chart() {
        for name in ["../escape.yaml", "/etc/passwd", "templates/../../x", ""] {
            let chart = StoredChart::from_json(json!({
                "metadata": {"name": "web", "version": "1"},
                "templates": [{"name": name, "data": ""}]
            }))
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            assert!(chart.write_to(dir.path()).is_err(), "{name}");
        }
    }

    #[test]
    fn debug_hides_values() {
        let debug = format!("{:?}", chart(Value::Null));
        assert!(debug.contains("web-1.0.0"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }
}
