//! What characters an identifier is made of.
//!
//! This is a fact about the language, not about any one consumer, so it lives
//! here next to [`keyword_lookup`](crate::kind::keyword_lookup) rather than
//! inside the lexer. The lexer is the main caller, but anything that has to
//! agree with it on where an identifier begins and ends — the LSP's
//! completion prefix scan, most obviously — needs the same predicate. A
//! second, private copy is how the two drift apart.

/// Identifier-start character. Matches `LexicalParser.java:432–434`:
/// ASCII letters, underscore, plus the Latin-1 letter blocks.
///
/// Deliberately *not* `char::is_alphabetic`: upstream accepts a fixed set of
/// accented Latin letters and nothing else, so a wider predicate would let
/// this toolchain lex programs the official parser rejects.
#[must_use]
pub fn is_ident_start(c: char) -> bool {
    if c.is_ascii_alphabetic() || c == '_' {
        return true;
    }
    matches!(
        c,
        '\u{00C0}'..='\u{00D6}' // À–Ö
        | '\u{00D8}'..='\u{00DD}' // Ø–Ý
        | '\u{00E0}'..='\u{00F6}' // à–ö
        | '\u{00F8}'..='\u{00FD}' // ø–ý
        | '\u{0152}'..='\u{0153}' // Œ–œ
        | '\u{00FF}'                // ÿ
    )
}

/// Identifier-continuation character: [`is_ident_start`] plus ASCII digits.
#[must_use]
pub fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letters_underscore_and_the_latin_1_blocks_start_an_ident() {
        for c in ['a', 'Z', '_', 'é', 'À', 'ÿ', 'œ', 'Œ'] {
            assert!(is_ident_start(c), "{c} should start an identifier");
        }
    }

    #[test]
    fn digits_continue_an_ident_but_do_not_start_one() {
        assert!(!is_ident_start('7'));
        assert!(is_ident_continue('7'));
    }

    /// The predicate mirrors upstream's fixed Latin-1 set, not Unicode at
    /// large — widening it would accept programs the official parser rejects.
    #[test]
    fn letters_outside_the_upstream_set_are_not_ident_characters() {
        for c in ['π', '∞', 'λ', 'Ж', '中', '.', ' '] {
            assert!(!is_ident_continue(c), "{c} should not be an ident char");
        }
    }
}
