//! Mapping an editor cursor to a field path, for completion.
//!
//! Indentation-based and tolerant of half-typed text: it reads only the
//! lines above the cursor, so the document need not parse.

use super::path::{FieldPath, PathSegment};

struct Frame {
    indent: usize,
    segment: PathSegment,
}

/// A key (or sequence item dash) found on a line, with its full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyLine {
    pub path: FieldPath,
    /// 0-based line in the whole text.
    pub line: usize,
    /// 0-based column (in bytes, = characters for ASCII keys) of the key.
    pub column: usize,
    /// Length of the key as written (1 for a `-`).
    pub len: usize,
}

#[derive(Default)]
pub(super) struct Walker {
    stack: Vec<Frame>,
    /// Inside a block scalar (`|`, `>`) started by a key at this indent.
    block_scalar: Option<usize>,
    /// Line being walked, for [`KeyLine`]s.
    line: usize,
    /// Every key walked, when recording.
    recorded: Option<Vec<KeyLine>>,
}

impl Walker {
    pub(super) fn recording() -> Self {
        Self {
            recorded: Some(Vec::new()),
            ..Self::default()
        }
    }

    pub(super) fn into_recorded(self) -> Vec<KeyLine> {
        self.recorded.unwrap_or_default()
    }

    /// Walks line number `number` (0-based) of the text.
    pub(super) fn walk(&mut self, number: usize, line: &str) {
        self.line = number;
        self.line(line);
    }

    fn record(&mut self, path: FieldPath, column: usize, len: usize) {
        let line = self.line;
        if let Some(recorded) = &mut self.recorded {
            recorded.push(KeyLine {
                path,
                line,
                column,
                len,
            });
        }
    }

    fn current(&self) -> FieldPath {
        FieldPath(
            self.stack
                .iter()
                .map(|frame| frame.segment.clone())
                .collect(),
        )
    }

    fn pop_to(&mut self, indent: usize) {
        while self
            .stack
            .last()
            .is_some_and(|frame| frame.indent >= indent)
        {
            self.stack.pop();
        }
    }

    fn line(&mut self, line: &str) {
        let indent = leading_spaces(line);
        let content = line[indent..].trim_end();
        if let Some(block) = self.block_scalar {
            if content.is_empty() || indent > block {
                return;
            }
            self.block_scalar = None;
        }
        if content.is_empty() || content.starts_with('#') {
            return;
        }
        self.content(indent, content);
    }

    fn content(&mut self, indent: usize, content: &str) {
        if let Some(rest) = content
            .strip_prefix('-')
            .filter(|r| r.is_empty() || r.starts_with(' '))
        {
            self.dash(indent);
            let gap = leading_spaces(rest);
            let rest = rest[gap..].trim_end();
            if !rest.is_empty() {
                self.content(indent + 1 + gap, rest);
            }
            return;
        }
        let Some((key, value)) = split_key(content) else {
            return;
        };
        self.pop_to(indent);
        if self.recorded.is_some() {
            let written = content.len() - content[key_end(content)..].len();
            self.record(self.current().key(&key), indent, written);
        }
        let value = value.split(" #").next().unwrap_or_default().trim();
        if value.is_empty() {
            self.stack.push(Frame {
                indent,
                segment: PathSegment::Key(key),
            });
        } else if value.starts_with(['|', '>']) {
            self.block_scalar = Some(indent);
        }
    }

    /// A sequence item at `indent`: the next index of the sequence there.
    fn dash(&mut self, indent: usize) {
        while self.stack.last().is_some_and(|frame| frame.indent > indent) {
            self.stack.pop();
        }
        let index = match self.stack.last() {
            Some(Frame {
                indent: at,
                segment: PathSegment::Index(previous),
            }) if *at == indent => {
                let next = previous + 1;
                self.stack.pop();
                next
            }
            _ => 0,
        };
        self.stack.push(Frame {
            indent,
            segment: PathSegment::Index(index),
        });
        if self.recorded.is_some() {
            self.record(self.current(), indent, 1);
        }
    }

    fn path(self) -> FieldPath {
        FieldPath(self.stack.into_iter().map(|frame| frame.segment).collect())
    }
}

/// The path of the mapping a key typed at `line`:`column` (0-based, in
/// characters) would belong to. Only the cursor's document is considered.
pub fn path_at(text: &str, line: usize, column: usize) -> FieldPath {
    let mut walker = Walker::default();
    for (number, current) in text.lines().enumerate() {
        if number == line {
            // Past the end of the line counts as spaces (virtual space).
            let mut prefix: String = current.chars().take(column).collect();
            let missing = column.saturating_sub(prefix.chars().count());
            prefix.extend(std::iter::repeat_n(' ', missing));
            let indent = leading_spaces(&prefix);
            let typed = prefix[indent..].trim_end();
            match typed
                .strip_prefix('-')
                .filter(|r| r.is_empty() || r.starts_with(' '))
            {
                Some(rest) => {
                    walker.dash(indent);
                    let gap = leading_spaces(rest);
                    walker.pop_to(indent + 1 + gap.max(1));
                }
                None => walker.pop_to(indent),
            }
            return walker.path();
        }
        if current.starts_with("---") || current.starts_with("...") {
            walker = Walker::default();
            continue;
        }
        walker.line(current);
    }
    // Cursor past the end: a new line at `column`.
    walker.pop_to(column);
    walker.path()
}

fn leading_spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// Byte length of the key as written at the start of `content` (quotes
/// included), assuming [`split_key`] accepted it.
fn key_end(content: &str) -> usize {
    if let Some(quote) = content.chars().next().filter(|c| *c == '"' || *c == '\'') {
        return content[1..]
            .find(quote)
            .map_or(content.len(), |end| end + 2);
    }
    match content.find(": ") {
        Some(at) => content[..at].trim_end().len(),
        None => content.trim_end().trim_end_matches(':').len(),
    }
}

/// `key: value` / `key:` / `"quoted key": value`.
fn split_key(content: &str) -> Option<(String, &str)> {
    if let Some(quote) = content.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let end = content[1..].find(quote)? + 1;
        let rest = content[end + 1..].strip_prefix(':')?;
        return (rest.is_empty() || rest.starts_with(' '))
            .then(|| (content[1..end].to_string(), rest));
    }
    let (key, value) = match content.find(": ") {
        Some(at) => (&content[..at], &content[at + 1..]),
        None => (content.strip_suffix(':')?, ""),
    };
    (!key.is_empty() && !key.starts_with(['{', '[', '#'])).then(|| (key.trim().to_string(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web
spec:
  replicas: 1
  template:
    spec:
      containers:
      - name: sidecar
        image: envoy
      - name: app
        args:
        - a
        - b
        command: |
          run: this
          now
        resources:

---
kind: ConfigMap
data:
  ";

    fn at(line: usize, column: usize) -> String {
        path_at(TEXT, line, column).to_string()
    }

    #[test]
    fn finds_enclosing_mapping() {
        assert_eq!(at(0, 0), ".");
        assert_eq!(at(3, 2), ".metadata");
        assert_eq!(at(5, 4), ".metadata.labels");
        assert_eq!(at(5, 2), ".metadata");
        assert_eq!(at(7, 2), ".spec");
        assert_eq!(at(12, 8), ".spec.template.spec.containers[0]");
        assert_eq!(at(14, 8), ".spec.template.spec.containers[1]");
        assert_eq!(at(16, 8), ".spec.template.spec.containers[1]");
        assert_eq!(at(16, 10), ".spec.template.spec.containers[1].args[1]");
        // After a block scalar whose text looks like a key.
        assert_eq!(at(21, 10), ".spec.template.spec.containers[1].resources");
        assert_eq!(at(21, 8), ".spec.template.spec.containers[1]");
        // Second document.
        assert_eq!(at(25, 2), ".data");
        assert_eq!(at(24, 0), ".");
    }

    #[test]
    fn typing_a_new_item() {
        let text = "spec:\n  containers:\n  - name: a\n  - ";
        assert_eq!(path_at(text, 3, 4).to_string(), ".spec.containers[1]");
        let text = "spec:\n  ports:\n    - port: 80\n      ";
        assert_eq!(path_at(text, 3, 6).to_string(), ".spec.ports[0]");
    }

    #[test]
    fn splits_keys() {
        assert_eq!(split_key("a: b"), Some(("a".into(), " b")));
        assert_eq!(split_key("a:"), Some(("a".into(), "")));
        assert_eq!(split_key("\"a.b\": c"), Some(("a.b".into(), " c")));
        assert_eq!(split_key("http://x"), None);
        assert_eq!(split_key("plain"), None);
    }
}
