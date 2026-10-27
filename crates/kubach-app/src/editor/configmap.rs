//! ConfigMap keys edited one by one in the YAML editor (#275).
//!
//! The keys view and the YAML text are two views of one object: the blocks
//! are built from the text, and written back into it (only `data` and
//! `binaryData` change) before switching to YAML or reviewing. Review and
//! apply then go the usual way: server-side dry run, diff, server-side
//! apply as `kubach`.

use std::path::Path;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::data_keys::{self, DataEntry, DataValue};
use kubach_i18n::t;
use kubach_ui::{DataKeyBlock, Tone};
use serde_json::Value;

use super::value::{ValueEditor, binary_text, content_label, unreadable};
use super::{Banner, YamlEditor, callout};

/// Largest file taken as a binary value (a whole ConfigMap is limited to 1 MiB).
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const KEYS_WIDTH: Pixels = px(860.);

pub(super) struct KeysView {
    blocks: Vec<KeyBlock>,
    next_id: usize,
    /// Entries as built (or last written back), to tell edits apart.
    initial: Vec<DataEntry>,
    immutable: bool,
    _subscriptions: Vec<Subscription>,
}

struct KeyBlock {
    id: usize,
    name: Entity<InputState>,
    /// The key and value as loaded; `None` for a new key.
    original: Option<DataEntry>,
    value: BlockValue,
}

enum BlockValue {
    Text(ValueEditor),
    /// `binaryData`: shown by size, replaced from a file.
    Binary {
        base64: String,
        file: Option<String>,
    },
}

impl KeyBlock {
    fn key(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_owned()
    }

    fn entry(&self, cx: &App) -> DataEntry {
        DataEntry {
            key: self.key(cx),
            value: match &self.value {
                BlockValue::Text(editor) => DataValue::Text(editor.text(cx)),
                BlockValue::Binary { base64, .. } => DataValue::Binary(base64.clone()),
            },
        }
    }
}

impl KeysView {
    pub fn entries(&self, cx: &App) -> Vec<DataEntry> {
        self.blocks.iter().map(|block| block.entry(cx)).collect()
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.entries(cx) != self.initial
    }

    /// Values that fail their check (`config.json: line 3: Invalid JSON…`).
    pub fn warnings(&self, cx: &App) -> Vec<String> {
        self.blocks
            .iter()
            .filter_map(|block| match &block.value {
                BlockValue::Text(editor) => editor
                    .problem
                    .as_ref()
                    .map(|problem| format!("{}: {problem}", block.key(cx))),
                BlockValue::Binary { .. } => None,
            })
            .collect()
    }
}

/// The ConfigMap's own editor kind: an existing core ConfigMap.
pub(super) fn is_configmap(target: &super::Target) -> bool {
    matches!(
        target,
        super::Target::Existing { kind, .. }
            if kind.api.group.is_empty() && kind.api.kind == "ConfigMap"
    )
}

impl YamlEditor {
    /// Builds the keys view from the editor text.
    pub(super) fn open_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let object = match data_keys::read_configmap(&self.text(cx)) {
            Ok(object) => object,
            Err(error) => {
                self.banner = Some(Banner::new(
                    Tone::Warning,
                    t!("editor-keys-unavailable", error = error.to_string()),
                ));
                cx.notify();
                return false;
            }
        };
        let entries = data_keys::configmap_entries(&object);
        let mut view = KeysView {
            blocks: Vec::new(),
            next_id: 0,
            initial: entries.clone(),
            immutable: object.get("immutable").and_then(Value::as_bool) == Some(true),
            _subscriptions: Vec::new(),
        };
        for entry in entries {
            Self::push_block(&mut view, Some(entry), window, cx);
        }
        self.keys = Some(view);
        cx.notify();
        true
    }

    /// Writes edited keys back into the YAML text.
    pub(super) fn sync_keys(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(keys) = &self.keys else {
            return Ok(());
        };
        if !keys.is_dirty(cx) {
            return Ok(());
        }
        let entries = keys.entries(cx);
        let text =
            data_keys::rewrite_configmap(&self.text(cx), &entries).map_err(|e| e.to_string())?;
        self.editor
            .update(cx, |state, cx| state.set_value(text, window, cx));
        if let Some(keys) = &mut self.keys {
            keys.initial = entries;
        }
        Ok(())
    }

    /// Switches between the keys view and the whole YAML.
    pub(super) fn set_keys_mode(
        &mut self,
        keys: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if keys == self.keys.is_some() || self.busy.is_some() {
            return;
        }
        if keys {
            self.keys_mode = self.open_keys(window, cx);
        } else {
            match self.sync_keys(window, cx) {
                Ok(()) => {
                    self.keys = None;
                    self.keys_mode = false;
                }
                Err(error) => self.banner = Some(Banner::new(Tone::Danger, error)),
            }
        }
        cx.notify();
    }

    fn push_block(
        view: &mut KeysView,
        entry: Option<DataEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        view.next_id += 1;
        let id = view.next_id;
        let key = entry.as_ref().map(|e| e.key.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(key.clone()));
        view._subscriptions
            .extend(crate::i18n::localized_placeholder(
                &name,
                || t!("editor-key-placeholder"),
                window,
                cx,
            ));
        view._subscriptions
            .push(cx.subscribe(&name, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.key_edited(id, cx);
                }
            }));
        let value = match entry.as_ref().map(|e| &e.value) {
            Some(DataValue::Binary(base64)) => BlockValue::Binary {
                base64: base64.clone(),
                file: None,
            },
            Some(DataValue::Text(text)) => {
                BlockValue::Text(ValueEditor::new(&key, text.clone(), window, cx))
            }
            None => BlockValue::Text(ValueEditor::new(&key, String::new(), window, cx)),
        };
        if let BlockValue::Text(editor) = &value {
            view._subscriptions.push(cx.subscribe(
                &editor.state,
                move |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.key_edited(id, cx);
                    }
                },
            ));
        }
        if entry.is_none() {
            name.update(cx, |name, cx| name.focus(window, cx));
        }
        view.blocks.push(KeyBlock {
            id,
            name,
            original: entry,
            value,
        });
    }

    fn key_edited(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(block) = self
            .keys
            .as_mut()
            .and_then(|keys| keys.blocks.iter_mut().find(|b| b.id == id))
        {
            let key = block.name.read(cx).value().trim().to_owned();
            if let BlockValue::Text(editor) = &mut block.value {
                editor.refresh(&key, cx);
            }
        }
        if self.banner.as_ref().is_some_and(|b| b.tone != Tone::Danger) {
            self.banner = None;
        }
        cx.notify();
    }

    fn add_key_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut keys) = self.keys.take() {
            Self::push_block(&mut keys, None, window, cx);
            self.keys = Some(keys);
        }
        cx.notify();
    }

    fn remove_key_block(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(keys) = &mut self.keys {
            keys.blocks.retain(|block| block.id != id);
        }
        cx.notify();
    }

    fn format_key_block(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(block) = self
            .keys
            .as_mut()
            .and_then(|keys| keys.blocks.iter_mut().find(|b| b.id == id))
            && let BlockValue::Text(editor) = &mut block.value
        {
            editor.format(window, cx);
        }
    }

    fn replace_binary(&mut self, id: usize, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = read_binary(&path);
            this.update(cx, |this, cx| {
                match result {
                    Ok((name, bytes)) => {
                        if let Some(block) = this
                            .keys
                            .as_mut()
                            .and_then(|keys| keys.blocks.iter_mut().find(|b| b.id == id))
                            && let DataValue::Binary(encoded) = DataValue::binary(&bytes)
                        {
                            block.value = BlockValue::Binary {
                                base64: encoded,
                                file: Some(name),
                            };
                        }
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_keys(&self, keys: &KeysView, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let editable = self.busy.is_none();
        let names: Vec<String> = keys.blocks.iter().map(|b| b.key(cx)).collect();
        let mut column = div().flex().flex_col().gap_3();
        if keys.immutable {
            column = column.child(callout(Tone::Info, t!("editor-configmap-immutable"), cx));
        }
        if keys.blocks.is_empty() {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t!("editor-no-keys")),
            );
        }
        for (ix, block) in keys.blocks.iter().enumerate() {
            let twice = names[..ix].contains(&names[ix]) || names[ix + 1..].contains(&names[ix]);
            column = column.child(self.render_key_block(block, twice, editable, cx));
        }
        if editable {
            column = column.child(
                div().child(
                    Button::new("configmap-add-key")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(t!("editor-add-key"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_key_block(window, cx)),
                        ),
                ),
            );
        }
        column = column.child(
            div()
                .text_xs()
                .text_color(muted)
                .whitespace_normal()
                .child(t!("editor-configmap-hint")),
        );
        div()
            .id("configmap-keys")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(div().max_w(KEYS_WIDTH).p_4().child(column))
            .into_any_element()
    }

    fn render_key_block(
        &self,
        block: &KeyBlock,
        twice: bool,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = block.id;
        let key = block.key(cx);
        let entry = block.entry(cx);
        let name = div()
            .w(px(300.))
            .child(Input::new(&block.name).small().disabled(!editable));
        let mut field = DataKeyBlock::new(("configmap-key", id), name);
        field = match &block.original {
            None => field.status(t!("editor-key-new"), Tone::Success),
            Some(original) if original.key != key => {
                field.status(t!("editor-key-renamed", key = &original.key), Tone::Info)
            }
            Some(original) if original.value != entry.value => {
                field.status(t!("editor-key-changed"), Tone::Info)
            }
            Some(_) => field,
        };
        let name_problem = data_keys::validate_key(&key)
            .err()
            .map(|e| e.to_string())
            .or_else(|| twice.then(|| t!("editor-key-twice", key = &key)));
        match &block.value {
            BlockValue::Text(editor) => {
                field = field.kind(content_label(editor.kind));
                if editor.kind == data_keys::ContentKind::Json {
                    field = field.action(
                        Button::new(("configmap-format", id))
                            .ghost()
                            .xsmall()
                            .icon(Lucide::Braces)
                            .label(t!("editor-format"))
                            .tooltip(t!("editor-format-tooltip"))
                            .disabled(!editable || !editor.can_format())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.format_key_block(id, window, cx)
                            })),
                    );
                }
                field = field.body(editor.element(!editable, cx));
                field = match name_problem {
                    Some(problem) => field.problem(Some(problem), Tone::Danger),
                    None => field.problem(editor.warning(), Tone::Warning),
                };
            }
            BlockValue::Binary { file, .. } => {
                let size = binary_text(entry.value.len());
                let line = match file {
                    Some(file) => t!("editor-binary-from-file", size = size, file = file),
                    None => t!("editor-binary-not-text", size = size),
                };
                field = field
                    .kind(t!("editor-content-binary"))
                    .action(
                        Button::new(("configmap-file", id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::File)
                            .label(t!("editor-replace-from-file"))
                            .disabled(!editable)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.replace_binary(id, cx)),
                            ),
                    )
                    .body(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(line),
                    )
                    .problem(name_problem, Tone::Danger);
            }
        }
        field
            .action(
                Button::new(("configmap-remove", id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Delete)
                    .tooltip(t!("editor-remove-key"))
                    .disabled(!editable)
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_key_block(id, cx))),
            )
            .into_any_element()
    }
}

fn read_binary(path: &Path) -> Result<(String, Vec<u8>), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = std::fs::metadata(path)
        .map_err(|e| unreadable(&name, &e))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(t!("editor-file-too-large-configmap", name = &name));
    }
    let bytes = std::fs::read(path).map_err(|e| unreadable(&name, &e))?;
    Ok((name, bytes))
}
