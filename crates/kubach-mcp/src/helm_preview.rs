//! What a helm preview tells the agent (#115).
//!
//! A manifest diff of a large chart does not fit a tool's answer, so
//! `preview: true` answers with a summary: objects added, changed and
//! removed, image changes, changed ConfigMap and Secret keys and changed
//! values. The diff itself stays in the session, split by object, for
//! `helm_preview_diff`: a filter by kind and name, then pages.
//!
//! Every page comes from the render of the preview: pages are cut from the
//! text kept here, nothing is rendered again, so they cannot disagree. The
//! kept text is masked before it is stored or cut, so a page border cannot
//! split a value away from its mask. A session keeps
//! [`MAX_KEPT_PREVIEWS`] of [`MAX_PREVIEW_BYTES`] at most; a preview that
//! fell out is an error that says so, never a new render.
//!
//! The confirmation window is not here: it gets the manifest diff from
//! `helm_tools` as before. What the user approves does not get shorter.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use kubach_core::apply::yaml;
use kubach_core::helm::manifest::{ChangeKind, ObjectChange, object_changes, unreadable_documents};
use kubach_core::helm::unified_diff;
use kubach_core::secrets::mask::{VALUE_FIELDS, is_secret_value};
use kubach_core::sync;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::helm_tools::{
    Elsewhere, authorize_elsewhere, looks_like_secret, mark_changed, mask_values, object_label,
};
use crate::masking::{MASK, mask_object, mask_text};
use crate::policy::{Denial, DenialCode, Request};
use crate::tools::{Ctx, ToolError, ToolOutput, authorize, parse};

/// Previews a session keeps; the oldest goes first.
pub(crate) const MAX_KEPT_PREVIEWS: usize = 4;
/// Diff text kept of one preview, at most.
pub(crate) const MAX_PREVIEW_BYTES: usize = 8 * 1024 * 1024;
/// One page of `helm_preview_diff`, at most.
pub(crate) const PAGE_BYTES: usize = 32 * 1024;
/// The summary's lists together, at most; the rows it cuts are counted.
pub(crate) const SUMMARY_BYTES: usize = 16 * 1024;
/// Rows of each list of the summary: the first that fits [`SUMMARY_BYTES`].
const ROW_CAPS: [usize; 4] = [50, 20, 8, 3];
/// A value longer than this stays out of the summary.
const SHORT_VALUE_BYTES: usize = 120;
/// How the values' diff is filed among the objects.
const VALUES_KIND: &str = "Values";

/// One part of the full diff: an object, or the values.
#[derive(Debug)]
pub(crate) struct DiffEntry {
    kind: String,
    name: String,
    text: String,
}

/// What a preview compares.
pub(crate) struct PreviewInput {
    /// The manifests, raw.
    pub old: String,
    pub new: String,
    pub old_label: String,
    pub new_label: String,
    pub namespace: String,
    /// Masked unified diff of the values, empty if none.
    pub values_diff: String,
    /// [`changed_values`].
    pub values: Vec<Value>,
    /// Labels of the objects `lookup` may misstate.
    pub lookup_objects: Vec<String>,
    /// `lookup` may misstate any object.
    pub lookup_whole: bool,
}

/// A preview's summary and the diff to keep.
#[derive(Debug)]
pub(crate) struct Compared {
    pub summary: Value,
    pub entries: Vec<DiffEntry>,
    /// Nothing was left out of `entries` for [`MAX_PREVIEW_BYTES`].
    pub complete: bool,
}

/// An object masked for the agent: [`mask_object`], and a ConfigMap value
/// that looks like a key or token under any name, as in Helm values.
fn masked_object(object: &Value) -> Value {
    let mut masked = object.clone();
    mask_object(&mut masked);
    if masked["kind"] == "ConfigMap"
        && let Some(data) = masked.get_mut("data").and_then(Value::as_object_mut)
    {
        for value in data.values_mut() {
            if let Value::String(text) = value
                && !text.starts_with(MASK)
                && looks_like_secret(text)
            {
                *value = Value::String(format!("{MASK} ({} bytes)", text.len()));
            }
        }
    }
    masked
}

/// The full diff of one object, both sides masked; a masked value that
/// changed is marked, or the two sides would look the same.
fn object_diff(input: &PreviewInput, change: &ObjectChange, label: &str) -> String {
    let old = change.old.as_ref().map(|old| old.expose());
    let new = change.new.as_ref().map(|new| new.expose());
    let old_masked = old.map(masked_object);
    let mut new_masked = new.map(masked_object);
    if let (Some(old), Some(new), Some(old_masked), Some(new_masked)) =
        (old, new, &old_masked, &mut new_masked)
    {
        mark_changed(old, new, old_masked, new_masked);
    }
    let text = |value: &Option<Value>| value.as_ref().map(yaml::to_yaml).unwrap_or_default();
    let diff = unified_diff(
        &text(&old_masked),
        &text(&new_masked),
        &format!("{}: {label}", input.old_label),
        &format!("{}: {label}", input.new_label),
    );
    let word = match change.kind {
        ChangeKind::Added => "added",
        ChangeKind::Changed => "changed",
        ChangeKind::Removed => "removed",
    };
    if diff.is_empty() {
        return format!("=== {word} {label}\nonly values that stay masked differ\n");
    }
    format!("=== {word} {label}\n{}", mask_text(&diff))
}

/// A value short enough for the summary, on one line.
fn short(value: &Value) -> Option<Value> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    (text.len() <= SHORT_VALUE_BYTES && !text.contains('\n')).then(|| match value {
        Value::String(_) => Value::String(mask_text(&text).into_owned()),
        other => other.clone(),
    })
}

/// `old` and `new` of a row, left out when absent or too long.
fn with_sides(row: &mut Map<String, Value>, old: Option<&Value>, new: Option<&Value>) {
    for (name, side) in [("old", old), ("new", new)] {
        if let Some(value) = side.and_then(short) {
            row.insert(name.to_owned(), value);
        }
    }
}

fn change_word(old: bool, new: bool) -> &'static str {
    match (old, new) {
        (false, _) => "added",
        (_, false) => "removed",
        _ => "changed",
    }
}

/// What differs between a release's values and the new ones, one row per
/// path: `path`, `change` and, unless too long, `old` and `new`, masked as
/// by [`mask_values`]. A masked value that changed says so by its row.
pub(crate) fn changed_values(old: &Value, new: &Value) -> Vec<Value> {
    let (mut old_masked, mut new_masked) = (old.clone(), new.clone());
    mask_values(&mut old_masked);
    mask_values(&mut new_masked);
    let mut rows = Vec::new();
    walk_values(
        "",
        (Some(old), Some(new)),
        (Some(&old_masked), Some(&new_masked)),
        &mut rows,
    );
    rows
}

type Sides<'a> = (Option<&'a Value>, Option<&'a Value>);

fn walk_values(path: &str, raw: Sides<'_>, masked: Sides<'_>, rows: &mut Vec<Value>) {
    if raw.0 == raw.1 {
        return;
    }
    if let (Some(Value::Object(old)), Some(Value::Object(new))) = raw {
        let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
        for key in keys {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            walk_values(
                &child,
                (old.get(key), new.get(key)),
                (
                    masked.0.and_then(|value| value.get(key)),
                    masked.1.and_then(|value| value.get(key)),
                ),
                rows,
            );
        }
        return;
    }
    let mut row = Map::new();
    row.insert("path".to_owned(), json!(path));
    row.insert(
        "change".to_owned(),
        json!(change_word(raw.0.is_some(), raw.1.is_some())),
    );
    with_sides(&mut row, masked.0, masked.1);
    rows.push(Value::Object(row));
}

/// The entries of `fields` of an object, by key.
fn entries<'a>(object: &'a Value, fields: &[&str]) -> BTreeMap<&'a str, &'a Value> {
    let mut found = BTreeMap::new();
    for field in fields {
        if let Some(Value::Object(map)) = object.get(field) {
            found.extend(map.iter().map(|(key, value)| (key.as_str(), value)));
        }
    }
    found
}

/// Keys of `fields` that differ between two objects, with what happened to
/// each: `(key, change)`.
fn changed_keys<'a>(
    old: &'a Value,
    new: &'a Value,
    fields: &[&str],
) -> Vec<(&'a str, &'static str)> {
    let (before, after) = (entries(old, fields), entries(new, fields));
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| before.get(key) != after.get(key))
        .map(|key| {
            (
                key,
                change_word(before.contains_key(key), after.contains_key(key)),
            )
        })
        .collect()
}

/// Rows of a changed Secret: key names and nothing about their values.
fn secret_rows(label: &str, old: &Value, new: &Value) -> Vec<Value> {
    changed_keys(old, new, &VALUE_FIELDS)
        .into_iter()
        .map(|(key, change)| json!({"object": label, "key": key, "change": change}))
        .collect()
}

/// Rows of a changed ConfigMap. Values come from the masked objects, and
/// never of `binaryData`.
fn config_map_rows(label: &str, old: &Value, new: &Value) -> Vec<Value> {
    let (old_masked, new_masked) = (masked_object(old), masked_object(new));
    changed_keys(old, new, &["data", "binaryData"])
        .into_iter()
        .map(|(key, change)| {
            let mut row = Map::new();
            row.insert("object".to_owned(), json!(label));
            row.insert("key".to_owned(), json!(key));
            row.insert("change".to_owned(), json!(change));
            with_sides(
                &mut row,
                old_masked["data"].get(key),
                new_masked["data"].get(key),
            );
            Value::Object(row)
        })
        .collect()
}

/// Every `image: <string>` of an object by where it is; a list item goes
/// by its name, so reordered containers are no change.
fn images(value: &Value, path: &str, found: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match child {
                    Value::String(image) if key == "image" => {
                        found.insert(child_path, image.clone());
                    }
                    _ if path.is_empty() && matches!(key.as_str(), "metadata" | "status") => {}
                    _ => images(child, &child_path, found),
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let child_path = match item["name"].as_str() {
                    Some(name) => format!("{path}[{name}]"),
                    None => format!("{path}[{index}]"),
                };
                images(item, &child_path, found);
            }
        }
        _ => {}
    }
}

/// Kinds where `image` is text or a schema, not what runs.
fn has_images(object: &Value) -> bool {
    !is_secret_value(object)
        && !matches!(
            object["kind"].as_str(),
            Some("ConfigMap" | "CustomResourceDefinition")
        )
}

/// What images an object runs that it did not. A removed object's are gone
/// with it, and its row says so.
fn image_rows(label: &str, old: Option<&Value>, new: &Value) -> Vec<Value> {
    if !has_images(new) {
        return Vec::new();
    }
    let (mut before, mut after) = (BTreeMap::new(), BTreeMap::new());
    if let Some(old) = old {
        images(old, "", &mut before);
    }
    images(new, "", &mut after);
    let mut rows = Vec::new();
    for (path, image) in &after {
        if before.get(path) == Some(image) {
            continue;
        }
        let mut row = Map::new();
        row.insert("object".to_owned(), json!(label));
        row.insert("path".to_owned(), json!(path));
        if let Some(old) = before.get(path) {
            row.insert("old".to_owned(), json!(mask_text(old)));
        }
        row.insert("new".to_owned(), json!(mask_text(image)));
        rows.push(Value::Object(row));
    }
    rows
}

/// The lists of the summary.
#[derive(Default)]
struct Rows {
    added: Vec<Value>,
    changed: Vec<Value>,
    removed: Vec<Value>,
    images: Vec<Value>,
    config_map_keys: Vec<Value>,
    secret_keys: Vec<Value>,
    values: Vec<Value>,
}

impl Rows {
    fn lists(&self) -> [(&'static str, &[Value]); 7] {
        [
            ("added", &self.added),
            ("changed", &self.changed),
            ("removed", &self.removed),
            ("images", &self.images),
            ("configMapKeys", &self.config_map_keys),
            ("secretKeys", &self.secret_keys),
            ("values", &self.values),
        ]
    }

    /// The lists, `cap` rows of each at most; `omitted` counts the rest.
    fn capped(&self, cap: usize) -> Map<String, Value> {
        let mut out = Map::new();
        let mut omitted = Map::new();
        for (name, rows) in self.lists() {
            out.insert(
                name.to_owned(),
                Value::Array(rows.iter().take(cap).cloned().collect()),
            );
            if rows.len() > cap {
                omitted.insert(name.to_owned(), json!(rows.len() - cap));
            }
        }
        if !omitted.is_empty() {
            out.insert("omitted".to_owned(), Value::Object(omitted));
        }
        out
    }

    /// The most rows that fit [`SUMMARY_BYTES`].
    fn fitted(&self) -> Map<String, Value> {
        ROW_CAPS
            .iter()
            .map(|cap| self.capped(*cap))
            .find(|lists| {
                serde_json::to_string(lists).map_or(0, |text| text.len()) <= SUMMARY_BYTES
            })
            .unwrap_or_else(|| self.capped(ROW_CAPS[ROW_CAPS.len() - 1]))
    }
}

/// Compares the two sides of a preview: the summary and the diff to keep.
/// Parses the manifests: call it on the blocking pool (#21).
pub(crate) fn compare(input: &PreviewInput) -> Compared {
    let found = object_changes(&input.old, &input.new);
    let mut rows = Rows {
        values: input.values.clone(),
        ..Rows::default()
    };
    let mut entries = Vec::new();
    let mut left = MAX_PREVIEW_BYTES;
    let mut complete = true;
    let mut keep = |kind: &str, name: &str, text: String, label: &str| {
        let text = if text.len() <= left {
            left -= text.len();
            text
        } else {
            complete = false;
            format!("=== {label}\nleft out: this preview's diff is too large to keep\n")
        };
        entries.push(DiffEntry {
            kind: kind.to_owned(),
            name: name.to_owned(),
            text,
        });
    };
    if !input.values_diff.trim().is_empty() {
        let text = format!("=== values\n{}", input.values_diff);
        keep(VALUES_KIND, "", text, "values");
    }
    for change in &found.changes {
        let label = object_label(&change.resource, &input.namespace);
        let affected = input.lookup_whole || input.lookup_objects.contains(&label);
        let mark = |mut row: Value| {
            if affected {
                row["lookupAffected"] = json!(true);
            }
            row
        };
        let old = change.old.as_ref().map(|old| old.expose());
        let new = change.new.as_ref().map(|new| new.expose());
        let mut object = json!({"object": label});
        if change.kind == ChangeKind::Removed && change.resource.keep {
            object["kept"] = json!(true);
        }
        let list = match change.kind {
            ChangeKind::Added => &mut rows.added,
            ChangeKind::Changed => &mut rows.changed,
            ChangeKind::Removed => &mut rows.removed,
        };
        list.push(mark(object));
        if let Some(new) = new {
            let found = image_rows(&label, old, new);
            rows.images.extend(found.into_iter().map(mark));
        }
        if let (Some(old), Some(new)) = (old, new) {
            if is_secret_value(new) {
                let found = secret_rows(&label, old, new);
                rows.secret_keys.extend(found.into_iter().map(mark));
            } else if new["kind"] == "ConfigMap" {
                let found = config_map_rows(&label, old, new);
                rows.config_map_keys.extend(found.into_iter().map(mark));
            }
        }
        keep(
            &change.resource.kind,
            &change.resource.name,
            object_diff(input, change, &label),
            &label,
        );
    }
    let mut summary = rows.fitted();
    let count = |kind: ChangeKind| found.changes.iter().filter(|c| c.kind == kind).count();
    summary.insert(
        "counts".to_owned(),
        json!({
            "added": count(ChangeKind::Added),
            "changed": count(ChangeKind::Changed),
            "removed": count(ChangeKind::Removed),
            "unchanged": found.unchanged,
        }),
    );
    let unreadable = unreadable_documents(&input.old) + unreadable_documents(&input.new);
    if unreadable > 0 {
        summary.insert("unreadableDocuments".to_owned(), json!(unreadable));
    }
    Compared {
        summary: Value::Object(summary),
        entries,
        complete,
    }
}

/// A preview kept for paging: only masked text.
#[derive(Debug)]
struct KeptPreview {
    id: String,
    release: String,
    namespace: String,
    /// Objects in other namespaces: reading the diff needs those rights
    /// as the preview did.
    elsewhere: Vec<Elsewhere>,
    entries: Vec<DiffEntry>,
    complete: bool,
    made: Instant,
}

/// The previews of a session, oldest first.
#[derive(Debug, Default)]
pub struct PreviewStore {
    next: u64,
    kept: VecDeque<Arc<KeptPreview>>,
}

impl PreviewStore {
    /// Keeps a preview, dropping the oldest beyond the limit, and says how
    /// the agent gets at it: `fullDiff` of the answer.
    pub(crate) fn keep(
        &mut self,
        release: &str,
        namespace: &str,
        elsewhere: Vec<Elsewhere>,
        compared: Compared,
    ) -> Value {
        self.next += 1;
        let id = format!("preview-{}", self.next);
        let text: String = compared.entries.iter().map(|e| e.text.as_str()).collect();
        let note = json!({
            "previewId": id,
            "bytes": text.len(),
            "pages": pages(&text).len(),
            "complete": compared.complete,
            "how": "helm_preview_diff with this previewId gives the masked diff of this very \
                    render, object by object: kind and name narrow it, page walks it.",
        });
        self.kept.push_back(Arc::new(KeptPreview {
            id,
            release: release.to_owned(),
            namespace: namespace.to_owned(),
            elsewhere,
            entries: compared.entries,
            complete: compared.complete,
            made: Instant::now(),
        }));
        while self.kept.len() > MAX_KEPT_PREVIEWS {
            self.kept.pop_front();
        }
        note
    }

    fn get(&self, id: &str) -> Option<Arc<KeptPreview>> {
        self.kept.iter().find(|kept| kept.id == id).cloned()
    }

    pub(crate) fn clear(&mut self) {
        self.kept.clear();
    }

    #[cfg(test)]
    pub(crate) fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }
}

/// `text` in pages of at most [`PAGE_BYTES`], cut after a line where one
/// fits; together they are exactly `text`.
pub(crate) fn pages(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while rest.len() > PAGE_BYTES {
        let mut end = PAGE_BYTES;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let cut = rest[..end].rfind('\n').map_or(end, |at| at + 1);
        found.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        found.push(rest);
    }
    found
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiffArgs {
    preview_id: String,
    kind: Option<String>,
    name: Option<String>,
    page: Option<usize>,
}

impl DiffArgs {
    fn matches(&self, entry: &DiffEntry) -> bool {
        self.kind
            .as_ref()
            .is_none_or(|kind| entry.kind.eq_ignore_ascii_case(kind.trim()))
            && self
                .name
                .as_ref()
                .is_none_or(|name| entry.name.contains(name.trim()))
    }
}

const SAME_RENDER: &str = "Every page of a previewId comes from the same render, kept by Kubach: \
     pages cannot disagree, and nothing was rendered for this call. It shows the moment of the \
     preview; a call without preview renders again, and the user approves that.";

/// `helm_preview_diff`: a page of a kept preview's diff. The same rights
/// as the preview itself: a right taken back since closes the diff too.
pub(crate) async fn preview_diff(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: DiffArgs = parse(args)?;
    let kept = sync::lock(ctx.session.helm_previews()).get(&a.preview_id);
    let Some(kept) = kept else {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "preview {} is not kept: a session keeps its last {MAX_KEPT_PREVIEWS} previews. \
                 Call helm_install or helm_upgrade with preview again; that is a new render, \
                 which may differ from the pages you already read",
                a.preview_id
            ),
        )
        .into());
    };
    let allowed = authorize(
        ctx,
        &Request::HelmChange {
            namespace: Some(kept.namespace.clone()),
            release: kept.release.clone(),
        },
    )
    .await?;
    authorize_elsewhere(ctx, &allowed, &kept.release, &kept.elsewhere, true).await?;
    let matched: Vec<&DiffEntry> = kept.entries.iter().filter(|e| a.matches(e)).collect();
    let text: String = matched.iter().map(|entry| entry.text.as_str()).collect();
    let all = pages(&text);
    let page = a.page.unwrap_or(1);
    if page == 0 || (page > all.len() && page != 1) {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "page {page} does not exist: this diff has {} page(s)",
                all.len()
            ),
        )
        .into());
    }
    let mut out = json!({
        "previewId": kept.id,
        "release": kept.release,
        "namespace": kept.namespace,
        "matchedObjects": matched.len(),
        "page": page,
        "pages": all.len(),
        "complete": kept.complete,
        "renderedSecondsAgo": kept.made.elapsed().as_secs(),
        "diff": all.get(page - 1).copied().unwrap_or_default(),
        "note": SAME_RENDER,
    });
    if page < all.len() {
        out["nextPage"] = json!(page + 1);
    }
    if matched.is_empty() {
        out["diff"] = json!(format!(
            "no change matches this filter: the summary lists what changed, and kind \
             {VALUES_KIND} is the values"
        ));
    }
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}
