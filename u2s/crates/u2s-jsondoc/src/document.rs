//! [`Document`] and [`Revision`] — the working output document the
//! Conversion Agent edits, and the monotonic counter that makes an edit
//! conditional on the version the agent actually saw.

use serde_json::Value;

/// A monotonically increasing version counter. `0` is a freshly created
/// document that has never been patched. There is no way to construct one
/// out of thin air (no `From<u64>`, deliberately) — the only source of a
/// `Revision` is a real [`Document`], so "the revision the agent has" always
/// traces back to an actual read of the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub const INITIAL: Revision = Revision(0);

    pub fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        // Saturating, not wrapping: a document patched u64::MAX times has
        // bigger problems, and a wrapped revision colliding with an earlier
        // one would silently defeat the whole point of the counter.
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for Revision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The working document: a JSON value plus the revision it is at. Every
/// mutation goes through [`crate::patch::apply`], which is the only place
/// `revision` advances — nothing else in this crate holds a `&mut
/// Document`.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    value: Value,
    revision: Revision,
}

impl Document {
    /// A freshly created document at [`Revision::INITIAL`].
    pub fn new(value: Value) -> Self {
        Self {
            value,
            revision: Revision::INITIAL,
        }
    }

    /// Reconstructs a document at a specific revision — for a caller
    /// reloading one from storage, where the revision already advanced
    /// across past patches this crate was not present for.
    pub fn at_revision(value: Value, revision: Revision) -> Self {
        Self { value, revision }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub(crate) fn advance(&mut self) {
        self.revision = self.revision.next();
    }

    pub(crate) fn value_mut(&mut self) -> &mut Value {
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_new_document_starts_at_revision_zero() {
        let doc = Document::new(json!({}));
        assert_eq!(doc.revision(), Revision::INITIAL);
        assert_eq!(doc.revision().get(), 0);
    }

    #[test]
    fn advance_increments_by_exactly_one() {
        let mut doc = Document::new(json!({}));
        doc.advance();
        assert_eq!(doc.revision().get(), 1);
        doc.advance();
        assert_eq!(doc.revision().get(), 2);
    }

    #[test]
    fn revision_at_u64_max_does_not_wrap() {
        let mut doc = Document::at_revision(json!({}), Revision(u64::MAX));
        doc.advance();
        assert_eq!(
            doc.revision().get(),
            u64::MAX,
            "must saturate, never wrap to 0"
        );
    }

    #[test]
    fn revisions_order_the_way_a_version_check_needs() {
        let a = Revision::INITIAL;
        let b = {
            let mut d = Document::new(json!({}));
            d.advance();
            d.revision()
        };
        assert!(a < b);
    }
}
