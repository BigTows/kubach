//! Field paths inside a document (`.spec.containers[0].image`).

use std::fmt;

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// A path from the document root, displayed in kubectl/SSA style.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldPath(pub Vec<PathSegment>);

impl FieldPath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn key(&self, key: &str) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Key(key.to_string()));
        path
    }

    pub fn index(&self, index: usize) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Index(index));
        path
    }

    /// Parses the display form: `.a.b[0]`, `a.b`, `.data["x.y"]`.
    pub fn parse(text: &str) -> Self {
        let mut segments = Vec::new();
        let mut rest = text.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("[\"") {
                let end = after.find("\"]").unwrap_or(after.len());
                segments.push(PathSegment::Key(after[..end].to_string()));
                rest = after.get(end + 2..).unwrap_or_default();
            } else if let Some(after) = rest.strip_prefix('[') {
                let end = after.find(']').unwrap_or(after.len());
                if let Ok(index) = after[..end].parse() {
                    segments.push(PathSegment::Index(index));
                }
                rest = after.get(end + 1..).unwrap_or_default();
            } else {
                let after = rest.strip_prefix('.').unwrap_or(rest);
                let end = after.find(['.', '[']).unwrap_or(after.len());
                if end > 0 {
                    segments.push(PathSegment::Key(after[..end].to_string()));
                }
                rest = &after[end..];
            }
        }
        Self(segments)
    }

    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The value at this path in `document`.
    pub fn get<'a>(&self, document: &'a Value) -> Option<&'a Value> {
        self.0
            .iter()
            .try_fold(document, |value, segment| match segment {
                PathSegment::Key(key) => value.get(key),
                PathSegment::Index(index) => value.get(*index),
            })
    }

    /// The path without its last segment; the root is its own parent.
    pub fn parent(&self) -> Self {
        let mut path = self.clone();
        path.0.pop();
        path
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str(".");
        }
        for segment in &self.0 {
            match segment {
                PathSegment::Key(key) if is_plain(key) => write!(f, ".{key}")?,
                PathSegment::Key(key) => write!(f, "[\"{key}\"]")?,
                PathSegment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}

fn is_plain(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_and_parses() {
        let path = FieldPath::root()
            .key("spec")
            .key("containers")
            .index(0)
            .key("image");
        assert_eq!(path.to_string(), ".spec.containers[0].image");
        assert_eq!(FieldPath::parse(".spec.containers[0].image"), path);
        assert_eq!(FieldPath::parse("spec.containers[0].image"), path);

        let quoted = FieldPath::root()
            .key("metadata")
            .key("labels")
            .key("app.kubernetes.io/name");
        let text = quoted.to_string();
        assert_eq!(text, ".metadata.labels[\"app.kubernetes.io/name\"]");
        assert_eq!(FieldPath::parse(&text), quoted);
        assert_eq!(FieldPath::root().to_string(), ".");
        assert!(FieldPath::parse(".").is_root());
    }

    #[test]
    fn gets_values() {
        let document = serde_json::json!({"spec": {"ports": [{"port": 80}]}});
        let port = FieldPath::parse(".spec.ports[0].port");
        assert_eq!(port.get(&document), Some(&serde_json::json!(80)));
        assert_eq!(FieldPath::parse(".spec.ports[1]").get(&document), None);
        assert_eq!(port.parent().to_string(), ".spec.ports[0]");
        assert_eq!(FieldPath::root().get(&document), Some(&document));
    }
}
