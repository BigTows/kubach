//! The code editor of one ConfigMap or Secret value (#275): highlighting and
//! a check picked by the key's extension or the content (JSON, YAML,
//! properties, text), and "Format" for JSON. A failed check is a warning:
//! the value is still saved, it is just a string.

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{Editor, EditorState, Position};
use gpui_kit::*;
use kubach_core::data_keys::{self, ContentKind, ValueProblem};
use kubach_core::secrets;
use kubach_i18n::t;
use kubach_ui::data_key_editor_height;

/// `12 bytes`, `1.5 KiB`: bytes in words, larger sizes in units.
pub(crate) fn size_text(len: usize) -> String {
    if len < 1024 {
        t!("editor-size-bytes", count = len)
    } else {
        secrets::describe_size(len)
    }
}

/// `binary data, 3 bytes`.
pub(crate) fn binary_text(len: usize) -> String {
    t!("editor-binary-data", size = size_text(len))
}

/// Why a picked file can't be read.
pub(super) fn unreadable(name: &str, error: &std::io::Error) -> String {
    t!(
        "editor-file-unreadable",
        name = name,
        error = error.to_string()
    )
}

/// Name of a value's content kind (JSON and YAML as they are).
pub(super) fn content_label(kind: ContentKind) -> String {
    match kind {
        ContentKind::Properties => t!("editor-content-properties"),
        ContentKind::Text => t!("editor-content-text"),
        other => other.label().to_owned(),
    }
}

pub(super) struct ValueEditor {
    pub state: Entity<EditorState>,
    pub kind: ContentKind,
    pub problem: Option<ValueProblem>,
}

impl ValueEditor {
    pub fn new(key: &str, text: String, window: &mut Window, cx: &mut App) -> Self {
        let kind = data_keys::detect(key, &text);
        let problem = data_keys::check(kind, &text);
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(kind.language())
                .line_number(true)
                .placeholder(t!("editor-value-placeholder"))
                .default_value(text)
        });
        let editor = Self {
            state,
            kind,
            problem,
        };
        editor.publish(cx);
        editor
    }

    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    /// Picks the kind again for `key` and rechecks the value.
    pub fn refresh(&mut self, key: &str, cx: &mut App) {
        let text = self.text(cx);
        let kind = data_keys::detect(key, &text);
        if kind != self.kind {
            self.kind = kind;
            self.state
                .update(cx, |state, cx| state.set_highlighter(kind.language(), cx));
        }
        self.problem = data_keys::check(kind, &text);
        self.publish(cx);
    }

    /// Indents a JSON value; an invalid one is left as is (its problem is
    /// already shown).
    pub fn format(&mut self, window: &mut Window, cx: &mut App) {
        if self.kind != ContentKind::Json {
            return;
        }
        if let Ok(formatted) = data_keys::format_json(&self.text(cx)) {
            self.state
                .update(cx, |state, cx| state.set_value(formatted, window, cx));
        }
    }

    pub fn can_format(&self) -> bool {
        self.kind == ContentKind::Json && self.problem.is_none()
    }

    /// The warning under the value.
    pub fn warning(&self) -> Option<String> {
        self.problem
            .as_ref()
            .map(|problem| t!("editor-value-warning", problem = problem.to_string()))
    }

    pub fn element(&self, disabled: bool, cx: &App) -> Editor {
        let lines = self.state.read(cx).value().lines().count();
        Editor::new(&self.state)
            .h(data_key_editor_height(lines))
            .disabled(disabled)
    }

    fn publish(&self, cx: &mut App) {
        let diagnostic = self.problem.as_ref().map(|problem| {
            let (line, column) = (problem.line as u32, problem.column as u32);
            Diagnostic::new(
                Position::new(line, column)..Position::new(line, column + 1),
                problem.message.clone(),
            )
            .with_severity(DiagnosticSeverity::Warning)
            .with_source("kubach")
        });
        self.state.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                set.extend(diagnostic);
            }
            cx.notify();
        });
    }
}
