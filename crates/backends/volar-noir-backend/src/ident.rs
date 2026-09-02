// @reliability: normal
// @ai: assisted

use volar_circuit_source::{is_ascii_ident, EmitError};

const KEYWORDS: &[&str] = &[
    "as",
    "assert",
    "assert_eq",
    "break",
    "call_data",
    "comptime",
    "constrained",
    "continue",
    "contract",
    "crate",
    "dual",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "global",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "mut",
    "pub",
    "return",
    "return_data",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unchecked",
    "unconstrained",
    "unsafe",
    "use",
    "where",
    "while",
];

pub fn sanitize_ident(name: &str) -> Result<String, EmitError> {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        return Err(EmitError::InvalidIdent {
            name: name.to_string(),
            reason: "empty after sanitizing".into(),
        });
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if KEYWORDS.contains(&s.as_str()) {
        s = format!("w_{s}");
    }
    if !is_ascii_ident(&s) || KEYWORDS.contains(&s.as_str()) {
        return Err(EmitError::InvalidIdent {
            name: name.to_string(),
            reason: "not a legal Noir identifier".into(),
        });
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_prefixed() {
        assert_eq!(sanitize_ident("fn").unwrap(), "w_fn");
        assert_eq!(sanitize_ident("let").unwrap(), "w_let");
    }

    #[test]
    fn digits_are_prefixed() {
        assert_eq!(sanitize_ident("2carry").unwrap(), "_2carry");
    }
}
