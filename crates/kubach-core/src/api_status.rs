//! What an error answer of the API server means, read from its HTTP code as
//! well as its reason.
//!
//! `Status::is_forbidden` and `Status::is_not_found` of `kube` compare only
//! the `reason`, and an answer that did not come from the API server itself
//! (a proxy in front of it, for example) carries the code without a reason.

use kube::core::Status;

/// The request was refused: HTTP 403 or the reason `Forbidden`.
pub fn is_forbidden(status: &Status) -> bool {
    status.code == 403 || status.is_forbidden()
}

/// The answer is "not found": HTTP 404 or the reason `NotFound`.
///
/// For explaining an error and for deciding whether a retry makes sense.
/// Code that treats "not found" as "the object is already gone" (deletion,
/// eviction) keeps to the reason: only the API server itself says that.
pub fn is_not_found(status: &Status) -> bool {
    status.code == 404 || status.is_not_found()
}

#[cfg(test)]
mod tests {
    use kube::core::Status;

    use super::{is_forbidden, is_not_found};

    fn status(code: u16, reason: &str) -> Status {
        Status {
            code,
            reason: reason.to_owned(),
            ..Status::default()
        }
    }

    #[test]
    fn an_answer_without_a_reason_is_read_by_its_code() {
        assert!(is_forbidden(&status(403, "")));
        assert!(is_not_found(&status(404, "")));
        assert!(!is_forbidden(&status(404, "")));
        assert!(!is_not_found(&status(403, "")));
        assert!(!is_forbidden(&status(500, "")));
        assert!(!is_forbidden(&status(0, "")));
    }

    #[test]
    fn an_answer_is_read_by_its_reason_too() {
        assert!(is_forbidden(&status(403, "Forbidden")));
        assert!(is_not_found(&status(404, "NotFound")));
        assert!(is_forbidden(&status(0, "Forbidden")));
        assert!(is_not_found(&status(0, "NotFound")));
        assert!(!is_forbidden(&status(409, "Conflict")));
    }
}
