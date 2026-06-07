//! `Serialize` / `Deserialize` for the diagnostic types.
//!
//! Wire format mirrors `doc/diagnostics.md` §4 — IDs and names are
//! both emitted so tooling can search either; spans are flattened to
//! `[start, end]` tuples.

use leek_span::Span;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use crate::{Applicability, Code, Diagnostic, Label, Severity, Suggestion, TextEdit};

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Code", 2)?;
        st.serialize_field("id", self.0)?;
        st.serialize_field("name", self.name())?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            id: String,
        }
        let raw = Raw::deserialize(d)?;
        // The id has to be a `'static` to match our struct; look it up in the
        // catalog. Unknown ids (forward-compat wire data, or a long-running
        // LSP fed arbitrary input) are interned through a capped, deduplicating
        // cache so a stream of them can't leak memory without bound — see
        // [`intern_unknown_code`].
        let id = crate::codes::CATALOG
            .iter()
            .find(|m| m.id == raw.id)
            .map_or_else(|| intern_unknown_code(&raw.id), |m| m.id);
        Ok(Code(id))
    }
}

/// Intern an *unknown* (non-catalog) diagnostic-code id to a `'static str`.
///
/// `Code` holds a `&'static str`, so an id absent from the catalog must be
/// promoted to `'static`. A naive `Box::leak` per deserialization leaks
/// unbounded memory when the same process deserializes many diagnostics (the
/// LSP runs for hours). This caches each *distinct* unknown id and leaks it at
/// most once; past a hard cap it returns a single shared `"UNKNOWN"` sentinel
/// so an adversarial stream of distinct ids can't grow memory forever.
fn intern_unknown_code(id: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    /// Generous ceiling — real code spaces are in the hundreds; this only
    /// bounds pathological/adversarial input.
    const MAX_INTERNED: usize = 4096;
    const OVERFLOW_SENTINEL: &str = "UNKNOWN";

    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = INTERNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = set.get(id) {
        return existing;
    }
    if set.len() >= MAX_INTERNED {
        return OVERFLOW_SENTINEL;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

impl Serialize for Severity {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Severity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "error" => Ok(Severity::Error),
            "warning" => Ok(Severity::Warning),
            "info" => Ok(Severity::Info),
            "hint" => Ok(Severity::Hint),
            other => Err(serde::de::Error::custom(format!(
                "unknown severity `{other}`"
            ))),
        }
    }
}

impl Serialize for Diagnostic {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Diagnostic", 6)?;
        st.serialize_field("code", &self.code)?;
        st.serialize_field("severity", &self.severity)?;
        st.serialize_field(
            "primary",
            &PrimarySer {
                span: self.span,
                label: &self.message,
            },
        )?;
        st.serialize_field("labels", &self.labels)?;
        st.serialize_field("notes", &self.notes)?;
        st.serialize_field("suggestions", &self.suggestions)?;
        st.end()
    }
}

#[derive(Serialize)]
struct PrimarySer<'a> {
    #[serde(serialize_with = "serialize_span")]
    span: Span,
    label: &'a str,
}

fn serialize_span<S: Serializer>(span: &Span, s: S) -> Result<S::Ok, S::Error> {
    let mut st = s.serialize_struct("Span", 3)?;
    st.serialize_field("source", &span.source.get())?;
    st.serialize_field("start", &span.start)?;
    st.serialize_field("end", &span.end)?;
    st.end()
}

impl Serialize for Label {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Label", 2)?;
        st.serialize_field(
            "span",
            &SpanWrap {
                source: self.span.source.get(),
                start: self.span.start,
                end: self.span.end,
            },
        )?;
        st.serialize_field("label", &self.message)?;
        st.end()
    }
}

#[derive(Serialize)]
struct SpanWrap {
    source: u32,
    start: u32,
    end: u32,
}

impl Serialize for Suggestion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Suggestion", 3)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("edits", &self.edits)?;
        st.serialize_field("applicability", &self.applicability)?;
        st.end()
    }
}

impl Serialize for TextEdit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("TextEdit", 2)?;
        st.serialize_field(
            "span",
            &SpanWrap {
                source: self.span.source.get(),
                start: self.span.start,
                end: self.span.end,
            },
        )?;
        st.serialize_field("replacement", &self.replacement)?;
        st.end()
    }
}

impl Serialize for Applicability {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            Applicability::MachineApplicable => "machine-applicable",
            Applicability::MaybeIncorrect => "maybe-incorrect",
            Applicability::HasPlaceholders => "has-placeholders",
            Applicability::Unspecified => "unspecified",
        };
        s.serialize_str(v)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Diagnostic, codes};
    use leek_span::{SourceId, Span};

    fn src() -> SourceId {
        SourceId::new(1).unwrap()
    }

    #[test]
    fn serializes_with_name() {
        let span = Span::new(src(), 10, 11);
        let diag = Diagnostic::error(codes::ASSIGNMENT_INCOMPATIBLE_TYPE, span, "type mismatch")
            .with_note("Leekscript v3+ is strict.");
        let json = serde_json::to_string(&diag).unwrap();
        assert!(json.contains("\"id\":\"E0250\""));
        assert!(json.contains("\"name\":\"AssignmentIncompatibleType\""));
        assert!(json.contains("\"severity\":\"error\""));
        assert!(json.contains("Leekscript v3+ is strict."));
    }

    #[test]
    fn known_code_roundtrips_to_catalog_static() {
        // A catalog id deserializes back to a real catalog code (resolving its
        // metadata), not a leaked copy — so `name()`/`meta()` work.
        let json = r#"{"id":"E0250","name":"whatever"}"#;
        let code: crate::Code = serde_json::from_str(json).unwrap();
        assert_eq!(code.id(), "E0250");
        assert_eq!(code.name(), codes::ASSIGNMENT_INCOMPATIBLE_TYPE.name());
        assert!(code.meta().is_some(), "a catalog code must resolve its metadata");
    }

    #[test]
    fn unknown_code_is_interned_once_not_leaked_per_deserialize() {
        // An unknown id is promoted to `'static` via the capped interner.
        // Deserializing the SAME unknown id twice must yield the identical
        // pointer — proving it is leaked at most once, not once per call.
        let json = r#"{"id":"E_UNKNOWN_TEST_CODE_42","name":"x"}"#;
        let a: crate::Code = serde_json::from_str(json).unwrap();
        let b: crate::Code = serde_json::from_str(json).unwrap();
        assert_eq!(a.id(), "E_UNKNOWN_TEST_CODE_42");
        assert!(
            std::ptr::eq(a.id(), b.id()),
            "unknown code should be interned (deduplicated), not re-leaked each time",
        );
    }
}
