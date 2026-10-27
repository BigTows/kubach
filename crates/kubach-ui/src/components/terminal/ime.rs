//! IME composition state of a terminal, apart from GPUI so it can be tested.
//!
//! The input handler's "document" is only the text being composed (preedit):
//! what the shell already got cannot be edited, so ranges from the platform
//! are UTF-16 offsets into the preedit. Preedit is drawn over the cursor and
//! reaches the process only when the IME commits it.

use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composition {
    /// Preedit text; never empty when set.
    marked: Option<String>,
    /// Caret or selection inside the preedit, in UTF-16 units.
    selected: Option<Range<usize>>,
}

impl Composition {
    pub fn marked(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    /// Length of the document (the preedit) in UTF-16 units.
    pub fn len_utf16(&self) -> usize {
        self.marked.as_deref().map_or(0, utf16_len)
    }

    /// The whole preedit, as the platform sees it.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.as_ref().map(|_| 0..self.len_utf16())
    }

    /// Caret or selection inside the preedit; the caret sits at the end when
    /// the IME did not say.
    pub fn selected_range(&self) -> Range<usize> {
        let len = self.len_utf16();
        self.selected
            .clone()
            .map(|range| range.start.min(len)..range.end.min(len))
            .unwrap_or(len..len)
    }

    /// Preedit text in `range` (UTF-16), with the range actually used.
    pub fn text_for_range(&self, range: Range<usize>) -> (String, Range<usize>) {
        let text = self.marked.as_deref().unwrap_or_default();
        let (start, end) = clamp(text, range);
        let bytes = byte_offset(text, start)..byte_offset(text, end);
        (text[bytes].to_owned(), start..end)
    }

    /// Commits `text` in place of `range` (the preedit when `None`) and ends
    /// the composition. Returns the text for the process: the committed
    /// result, which is everything left in the document. Empty text (a
    /// cancelled composition) returns `None`.
    pub fn commit(&mut self, range: Option<Range<usize>>, text: &str) -> Option<String> {
        let (result, _) = self.splice(range, text);
        self.marked = None;
        self.selected = None;
        (!result.is_empty()).then_some(result)
    }

    /// Replaces `range` (the preedit when `None`) with `text` and keeps the
    /// result as preedit, with `selected` relative to `text`. Empty preedit
    /// ends the composition without sending anything.
    pub fn mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let (result, start) = self.splice(range, text);
        if result.is_empty() {
            self.marked = None;
            self.selected = None;
            return;
        }
        self.selected = selected.map(|range| start + range.start..start + range.end);
        self.marked = Some(result);
    }

    /// Accepts the preedit as it is (`unmarkText`): returns it for the
    /// process.
    pub fn unmark(&mut self) -> Option<String> {
        self.selected = None;
        self.marked.take()
    }

    /// Drops the preedit without sending it (focus lost, click).
    pub fn cancel(&mut self) -> bool {
        self.selected = None;
        self.marked.take().is_some()
    }

    /// The document after replacing `range` with `text`, and the UTF-16
    /// offset where `text` starts.
    fn splice(&self, range: Option<Range<usize>>, text: &str) -> (String, usize) {
        let doc = self.marked.as_deref().unwrap_or_default();
        let range = range
            .or_else(|| self.marked_range())
            .unwrap_or_else(|| self.selected_range());
        let (start, end) = clamp(doc, range);
        let mut result = String::with_capacity(doc.len() + text.len());
        result.push_str(&doc[..byte_offset(doc, start)]);
        result.push_str(text);
        result.push_str(&doc[byte_offset(doc, end)..]);
        (result, start)
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `range` limited to `text` and ordered.
fn clamp(text: &str, range: Range<usize>) -> (usize, usize) {
    let len = utf16_len(text);
    let start = range.start.min(len);
    (start, range.end.clamp(start, len))
}

/// Byte offset of the UTF-16 offset `utf16` in `text`, rounded down to a
/// character boundary.
pub fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units + c.len_utf16() > utf16 {
            return index;
        }
        units += c.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::{Composition, byte_offset};

    #[test]
    fn preedit_is_kept_until_commit() {
        let mut ime = Composition::default();
        ime.mark(None, "n", Some(1..1));
        ime.mark(None, "ni", Some(2..2));
        assert_eq!(ime.marked(), Some("ni"));
        assert_eq!(ime.marked_range(), Some(0..2));
        assert_eq!(ime.selected_range(), 2..2);
        assert_eq!(ime.commit(None, "你"), Some("你".to_owned()));
        assert!(!ime.is_composing());
        // A second commit of nothing sends nothing: no double input.
        assert_eq!(ime.commit(None, ""), None);
    }

    #[test]
    fn plain_insert_without_preedit() {
        let mut ime = Composition::default();
        assert_eq!(ime.commit(None, "é"), Some("é".to_owned()));
        // Ranges the platform keeps for text already sent are ignored.
        assert_eq!(ime.commit(Some(0..1), "ü"), Some("ü".to_owned()));
    }

    #[test]
    fn cancel_and_empty_preedit_send_nothing() {
        let mut ime = Composition::default();
        ime.mark(None, "かな", None);
        // Linux and Windows cancel by replacing the preedit with nothing.
        assert_eq!(ime.commit(ime.marked_range(), ""), None);
        assert!(!ime.is_composing());

        ime.mark(None, "´", None);
        ime.mark(None, "", None);
        assert!(!ime.is_composing());

        ime.mark(None, "x", None);
        assert!(ime.cancel());
        assert!(!ime.cancel());
    }

    #[test]
    fn unmark_accepts_the_preedit() {
        let mut ime = Composition::default();
        ime.mark(None, "한", None);
        assert_eq!(ime.unmark(), Some("한".to_owned()));
        assert_eq!(ime.unmark(), None);
    }

    #[test]
    fn partial_replacement_uses_utf16_offsets() {
        let mut ime = Composition::default();
        // `𝄞` is two UTF-16 units.
        ime.mark(None, "a𝄞b", Some(4..4));
        assert_eq!(ime.len_utf16(), 4);
        ime.mark(Some(3..4), "c", Some(1..1));
        assert_eq!(ime.marked(), Some("a𝄞c"));
        assert_eq!(ime.selected_range(), 4..4);
        assert_eq!(ime.text_for_range(1..3), ("𝄞".to_owned(), 1..3));
        assert_eq!(ime.text_for_range(3..10), ("c".to_owned(), 3..4));
        assert_eq!(byte_offset("a𝄞c", 3), 5);
    }
}
