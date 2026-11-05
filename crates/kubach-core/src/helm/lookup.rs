//! Which objects of a chart depend on `lookup` (#114).
//!
//! `helm template` renders without the cluster, where `lookup` finds
//! nothing: a generated TLS Secret looks recreated on every upgrade. The
//! chart's sources show which templates call it, directly or through
//! `include`, and the `# Source:` comments of the render which objects came
//! from those templates. Everything here is text work: nothing is rendered
//! or written to disk.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};

use flate2::read::GzDecoder;

use super::manifest::{ManifestResource, manifest_documents};

/// What a chart archive may unpack to; a bigger one is not read.
const ARCHIVE_BUDGET: u64 = 128 * 1024 * 1024;
/// Subcharts inside subcharts.
const ARCHIVE_DEPTH: usize = 8;

/// Where a chart depends on `lookup`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookupUse {
    /// Templates that render with `lookup`, as in `# Source:` comments
    /// (`chart/templates/secret.yaml`).
    pub templates: BTreeSet<String>,
    /// `lookup` may reach templates that cannot be named: the chart also
    /// has an `include` of a computed name.
    pub uncertain: bool,
}

impl LookupUse {
    pub fn is_used(&self) -> bool {
        self.uncertain || !self.templates.is_empty()
    }
}

/// The objects of a render that depend on `lookup`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookupObjects {
    pub objects: Vec<ManifestResource>,
    /// Which objects cannot be told: any of the render may depend on it.
    pub whole_render: bool,
}

/// A piece of a `{{ … }}` action.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
}

/// The actions of a template, without comments and with string literals
/// kept apart, so that neither `{{/* lookup */}}` nor `{{ "lookup" }}`
/// counts as a call.
fn actions(text: &str) -> Vec<Vec<Token>> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let inner = rest.strip_prefix('-').unwrap_or(rest).trim_start();
        if let Some(comment) = inner.strip_prefix("/*") {
            let Some(end) = comment.find("*/") else {
                break;
            };
            rest = &comment[end + 2..];
            continue;
        }
        let (action, after) = tokens(inner);
        found.push(action);
        rest = after;
    }
    found
}

/// The tokens of an action up to its `}}`, and what follows it.
fn tokens(text: &str) -> (Vec<Token>, &str) {
    fn flush(word: &mut String, tokens: &mut Vec<Token>) {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    }
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '}' if text[at..].starts_with("}}") => {
                flush(&mut word, &mut tokens);
                return (tokens, &text[at + 2..]);
            }
            '"' | '\'' | '`' => {
                flush(&mut word, &mut tokens);
                let mut literal = String::new();
                while let Some((_, inner)) = chars.next() {
                    if inner == c {
                        break;
                    }
                    if inner == '\\' && c != '`' {
                        if let Some((_, escaped)) = chars.next() {
                            literal.push(escaped);
                        }
                        continue;
                    }
                    literal.push(inner);
                }
                tokens.push(Token::Text(literal));
            }
            c if c.is_whitespace() || "()|,=:".contains(c) => flush(&mut word, &mut tokens),
            c => word.push(c),
        }
    }
    (tokens, "")
}

/// A template file or a `define`: whether it calls `lookup`, and what it
/// includes.
#[derive(Debug, Default)]
struct Body {
    calls_lookup: bool,
    includes: BTreeSet<String>,
}

impl Body {
    fn reaches_lookup(&self, tainted: &BTreeSet<String>) -> bool {
        self.calls_lookup || self.includes.iter().any(|name| tainted.contains(name))
    }
}

/// Files helm renders into objects: not partials (`_helpers.tpl`) and not
/// `NOTES.txt`.
fn renders_objects(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    !name.starts_with('_') && name != "NOTES.txt"
}

/// What the templates hold, by file and by `define`.
#[derive(Debug, Default)]
struct Scan<'a> {
    defines: BTreeMap<String, Body>,
    files: BTreeMap<&'a str, Body>,
    computed_include: bool,
}

impl<'a> Scan<'a> {
    /// The body an action belongs to: the innermost open `define`, or the
    /// file.
    fn owner(&mut self, path: &'a str, blocks: &[Option<String>]) -> &mut Body {
        match blocks.iter().rev().flatten().next() {
            Some(define) => self.defines.entry(define.clone()).or_default(),
            None => self.files.entry(path).or_default(),
        }
    }

    fn read(&mut self, path: &'a str, text: &str) {
        self.files.entry(path).or_default();
        // The open blocks: a name for `define` and `block`, else `None`.
        let mut blocks: Vec<Option<String>> = Vec::new();
        for action in actions(text) {
            let first = match action.first() {
                Some(Token::Word(word)) => word.as_str(),
                _ => "",
            };
            match first {
                "define" | "block" => {
                    let name = match action.get(1) {
                        Some(Token::Text(name)) => Some(name.clone()),
                        _ => None,
                    };
                    if let (true, Some(name)) = (first == "block", &name) {
                        // A block renders where it stands.
                        let owner = self.owner(path, &blocks);
                        owner.includes.insert(name.clone());
                    }
                    blocks.push(name);
                    continue;
                }
                "if" | "range" | "with" => blocks.push(None),
                "end" => {
                    blocks.pop();
                    continue;
                }
                _ => {}
            }
            self.calls(path, &blocks, &action);
        }
    }

    /// Notes the `lookup` and `include` calls of one action.
    fn calls(&mut self, path: &'a str, blocks: &[Option<String>], action: &[Token]) {
        for (at, token) in action.iter().enumerate() {
            let Token::Word(word) = token else {
                continue;
            };
            match word.as_str() {
                "lookup" => self.owner(path, blocks).calls_lookup = true,
                "include" | "template" => match action.get(at + 1) {
                    Some(Token::Text(name)) => {
                        self.owner(path, blocks).includes.insert(name.clone());
                    }
                    _ => self.computed_include = true,
                },
                _ => {}
            }
        }
    }

    /// The `define`s that reach `lookup`, through each other too.
    fn tainted(&self) -> BTreeSet<String> {
        let mut tainted = BTreeSet::new();
        loop {
            let more: Vec<String> = self
                .defines
                .iter()
                .filter(|(name, body)| !tainted.contains(*name) && body.reaches_lookup(&tainted))
                .map(|(name, _)| name.clone())
                .collect();
            if more.is_empty() {
                return tainted;
            }
            tainted.extend(more);
        }
    }
}

/// Finds the templates that depend on `lookup` among `files`, each a path
/// as in `# Source:` comments and its text. Helpers count through the
/// `include`s that reach them, across subcharts, as Go templates share one
/// namespace.
pub fn lookup_use(files: &[(String, String)]) -> LookupUse {
    let mut scan = Scan::default();
    for (path, text) in files {
        scan.read(path, text);
    }
    let tainted = scan.tainted();
    let templates: BTreeSet<String> = scan
        .files
        .iter()
        .filter(|(path, body)| renders_objects(path) && body.reaches_lookup(&tainted))
        .map(|(path, _)| (*path).to_owned())
        .collect();
    LookupUse {
        uncertain: scan.computed_include && (!templates.is_empty() || !tainted.is_empty()),
        templates,
    }
}

/// The objects of `rendered` (`helm template`'s output) from the templates
/// of `used`. The whole render counts when the mapping cannot be trusted:
/// the use is uncertain, a document has no `# Source:`, or a template of a
/// subchart is missing from the render, which an alias of the subchart
/// does as well as a disabled subchart.
pub fn lookup_objects(rendered: &str, used: &LookupUse) -> LookupObjects {
    let documents = manifest_documents(rendered);
    let sources: BTreeSet<&str> = documents
        .iter()
        .filter_map(|(source, _)| source.as_deref())
        .collect();
    let lost_subchart = used
        .templates
        .iter()
        .any(|path| path.contains("/charts/") && !sources.contains(path.as_str()));
    let unnamed = documents.iter().any(|(source, _)| source.is_none());
    let whole_render = used.is_used() && (used.uncertain || lost_subchart || unnamed);
    LookupObjects {
        whole_render,
        objects: documents
            .into_iter()
            .filter(|(source, _)| {
                source
                    .as_ref()
                    .is_some_and(|source| used.templates.contains(source))
            })
            .map(|(_, resource)| resource)
            .collect(),
    }
}

/// A file of a chart archive.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Template,
    Subchart,
    Other,
}

/// `chart/templates/…` and `chart/charts/sub/templates/…` are templates,
/// `chart/charts/sub-1.0.0.tgz` a subchart.
fn entry_kind(path: &str) -> Entry {
    let parts: Vec<&str> = path.split('/').collect();
    let mut at = 1;
    while at < parts.len() {
        match parts[at] {
            "templates" if at + 1 < parts.len() => return Entry::Template,
            "charts" if at + 2 == parts.len() => {
                return if parts[at + 1].ends_with(".tgz") {
                    Entry::Subchart
                } else {
                    Entry::Other
                };
            }
            "charts" => at += 2,
            _ => return Entry::Other,
        }
    }
    Entry::Other
}

/// The templates of a chart archive as `helm pull` saves it, subcharts
/// included, each with its path as in `# Source:` comments. Read in memory,
/// so a name in the archive reaches no path on disk. An error (a broken
/// archive, or one bigger than the budget) means the templates are unknown.
pub fn archive_templates(archive: &[u8]) -> io::Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    let mut budget = ARCHIVE_BUDGET;
    read_archive(archive, "", 0, &mut budget, &mut files)?;
    Ok(files)
}

fn read_archive(
    archive: &[u8],
    prefix: &str,
    depth: usize,
    budget: &mut u64,
    files: &mut Vec<(String, String)>,
) -> io::Result<()> {
    let too_big = || io::Error::new(io::ErrorKind::InvalidData, "the chart is too big to read");
    if depth > ARCHIVE_DEPTH {
        return Err(too_big());
    }
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        *budget = budget.checked_sub(entry.size()).ok_or_else(too_big)?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let kind = entry_kind(&path);
        if kind == Entry::Other {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        if kind == Entry::Template {
            let text = String::from_utf8_lossy(&data).into_owned();
            files.push((format!("{prefix}{path}"), text));
        } else {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            let inner = format!("{prefix}{parent}/");
            read_archive(&data, &inner, depth + 1, budget, files)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect()
    }

    fn templates(used: &LookupUse) -> Vec<&str> {
        used.templates.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_direct_call_marks_its_template() {
        let used = lookup_use(&files(&[
            (
                "c/templates/secret.yaml",
                r#"{{- $old := lookup "v1" "Secret" .Release.Namespace "tls" }}"#,
            ),
            (
                "c/templates/parens.yaml",
                r#"x: {{ (lookup "v1" "ConfigMap" "a" "b").data }}"#,
            ),
            ("c/templates/plain.yaml", "kind: ConfigMap\n"),
        ]));
        assert_eq!(
            templates(&used),
            ["c/templates/parens.yaml", "c/templates/secret.yaml"]
        );
        assert!(!used.uncertain);
    }

    #[test]
    fn words_that_only_look_like_a_call_do_not_count() {
        let used = lookup_use(&files(&[
            ("c/templates/a.yaml", "# lookup in YAML\n{{/* lookup */}}\n"),
            (
                "c/templates/b.yaml",
                r#"{{ "lookup" }}{{ .Values.lookup }}"#,
            ),
            (
                "c/templates/c.yaml",
                "{{- /* lookup */ -}}\n{{ $lookup }}\n",
            ),
            ("c/templates/d.yaml", r#"{{ `lookup` }} {{ "a\"lookup" }}"#),
        ]));
        assert!(!used.is_used(), "{used:?}");
    }

    #[test]
    fn a_helper_counts_through_its_includes() {
        let used = lookup_use(&files(&[
            (
                "c/templates/_helpers.tpl",
                r#"
{{- define "c.labels" -}}
{{- if .x }}a{{ end }}
{{- end -}}
{{- define "c.cert" -}}
{{- if .x }}{{ lookup "v1" "Secret" "a" "b" }}{{ end }}
{{- end -}}
{{- define "c.wrapped" -}}
{{ include "c.cert" . }}
{{- end -}}
"#,
            ),
            (
                "c/templates/secret.yaml",
                r#"a: {{ include "c.wrapped" . | b64enc }}"#,
            ),
            ("c/templates/deploy.yaml", r#"{{ include "c.labels" . }}"#),
            ("c/templates/job.yaml", r#"{{ template "c.cert" . }}"#),
        ]));
        assert_eq!(
            templates(&used),
            ["c/templates/job.yaml", "c/templates/secret.yaml"]
        );
        assert!(!used.uncertain);
    }

    #[test]
    fn a_helper_of_a_subchart_reaches_the_parent() {
        let used = lookup_use(&files(&[
            (
                "c/charts/common/templates/_secrets.tpl",
                r#"{{ define "common.pw" }}{{ lookup "v1" "Secret" "a" "b" }}{{ end }}"#,
            ),
            ("c/templates/secret.yaml", r#"{{ include "common.pw" . }}"#),
        ]));
        assert_eq!(templates(&used), ["c/templates/secret.yaml"]);
    }

    #[test]
    fn an_unused_helper_marks_nothing_and_notes_are_no_objects() {
        let used = lookup_use(&files(&[
            (
                "c/templates/_helpers.tpl",
                r#"{{ define "c.unused" }}{{ lookup "v1" "Secret" "a" "b" }}{{ end }}"#,
            ),
            ("c/templates/NOTES.txt", r#"{{ include "c.unused" . }}"#),
        ]));
        assert!(!used.is_used(), "{used:?}");
    }

    #[test]
    fn a_computed_include_makes_the_use_uncertain_only_with_lookup() {
        let computed = (
            "c/templates/deploy.yaml",
            r#"{{ include (print $.Template.BasePath "/secret.yaml") . | sha256sum }}"#,
        );
        assert!(!lookup_use(&files(&[computed])).is_used());
        let used = lookup_use(&files(&[
            computed,
            (
                "c/templates/secret.yaml",
                r#"{{ lookup "v1" "Secret" "a" "b" }}"#,
            ),
        ]));
        assert!(used.uncertain);
        assert_eq!(templates(&used), ["c/templates/secret.yaml"]);
    }

    const RENDERED: &str = "---\n# Source: c/templates/secret.yaml\napiVersion: v1\nkind: Secret\n\
         metadata:\n  name: tls\n---\n# Source: c/templates/plain.yaml\napiVersion: v1\n\
         kind: ConfigMap\nmetadata:\n  name: plain\n";

    fn used(templates: &[&str]) -> LookupUse {
        LookupUse {
            templates: templates.iter().map(|path| (*path).to_owned()).collect(),
            uncertain: false,
        }
    }

    fn names(objects: &LookupObjects) -> Vec<String> {
        objects
            .objects
            .iter()
            .map(|object| format!("{} {}", object.kind, object.name))
            .collect()
    }

    #[test]
    fn objects_come_from_the_templates_by_their_source() {
        let found = lookup_objects(RENDERED, &used(&["c/templates/secret.yaml"]));
        assert_eq!(names(&found), ["Secret tls"]);
        assert!(!found.whole_render);
        // Off by a value: nothing of the render to mark.
        let found = lookup_objects(RENDERED, &used(&["c/templates/off.yaml"]));
        assert!(found.objects.is_empty());
        assert!(!found.whole_render);
        assert_eq!(
            lookup_objects(RENDERED, &LookupUse::default()),
            LookupObjects::default()
        );
    }

    #[test]
    fn the_whole_render_counts_when_the_mapping_fails() {
        let uncertain = LookupUse {
            uncertain: true,
            ..used(&["c/templates/secret.yaml"])
        };
        let found = lookup_objects(RENDERED, &uncertain);
        assert!(found.whole_render);
        assert_eq!(names(&found), ["Secret tls"]);
        // An alias renames the subchart in `# Source:`.
        let aliased = used(&["c/charts/db/templates/secret.yaml"]);
        assert!(lookup_objects(RENDERED, &aliased).whole_render);
        // A render without comments.
        let bare = "---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: tls\n";
        let found = lookup_objects(bare, &used(&["c/templates/secret.yaml"]));
        assert!(found.whole_render);
    }

    fn tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn an_archive_gives_its_templates_and_those_of_its_subcharts() {
        let inner = tgz(&[
            ("common/Chart.yaml", b"name: common\n"),
            ("common/templates/_helpers.tpl", b"inner"),
        ]);
        let archive = tgz(&[
            ("c/Chart.yaml", b"name: c\n"),
            ("c/values.yaml", b"password: hunter2\n"),
            ("c/templates/secret.yaml", b"outer"),
            ("c/templates/sub/dir.yaml", b"deep"),
            ("c/charts/db/templates/x.yaml", b"unpacked"),
            ("c/charts/common-1.0.0.tgz", &inner),
            ("c/files/templates/not.yaml", b"not a template"),
        ]);
        let files = archive_templates(&archive).unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "c/templates/secret.yaml",
                "c/templates/sub/dir.yaml",
                "c/charts/db/templates/x.yaml",
                "c/charts/common/templates/_helpers.tpl",
            ]
        );
        assert_eq!(files[3].1, "inner");
    }

    #[test]
    fn a_broken_archive_is_an_error() {
        assert!(archive_templates(b"not a chart").is_err());
    }
}
