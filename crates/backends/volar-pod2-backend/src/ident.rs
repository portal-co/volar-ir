// @reliability: normal
// @ai: assisted

use volar_circuit_source::{is_ascii_ident, EmitError};

const KEYWORDS: &[&str] = &[
    "private",
    "true",
    "false",
    "record",
    "AND",
    "OR",
    "REQUEST",
    "Equal",
    "NotEqual",
    "Lt",
    "LtEq",
    "Sum",
    "Product",
    "Max",
    "Contains",
    "NotContains",
    "DictContains",
    "ArrayContains",
    "ArrayUpdate",
    "SetContains",
    "use",
    "module",
    "intro",
    "from",
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
            reason: "not a legal Podlang identifier".into(),
        });
    }
    Ok(s)
}
