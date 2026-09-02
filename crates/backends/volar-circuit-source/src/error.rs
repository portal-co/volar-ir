// @reliability: normal
// @ai: assisted

use volar_ir::ir::IRVarId;

/// Why circuit-source emission failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// The backend (or walker) cannot represent this construct.
    Unsupported {
        what: String,
        hint: Option<String>,
    },
    /// An identifier is not legal in the target language.
    InvalidIdent { name: String, reason: String },
    /// Two distinct wires sanitized to the same identifier.
    NameCollision { name: String },
    /// An SSA id was referenced outside the circuit's var space.
    UnknownVar { id: IRVarId },
    /// A storage address is wider than the backend can fold to an integer.
    AddressTooWide { bits: usize },
    /// A Volar IR type cannot be lowered to the target.
    TypeUnsupported { ty: String },
}

impl core::fmt::Display for EmitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EmitError::Unsupported { what, hint } => match hint {
                Some(h) => write!(f, "unsupported: {what} ({h})"),
                None => write!(f, "unsupported: {what}"),
            },
            EmitError::InvalidIdent { name, reason } => {
                write!(f, "invalid identifier `{name}`: {reason}")
            }
            EmitError::NameCollision { name } => {
                write!(f, "wire name `{name}` was assigned to more than one var")
            }
            EmitError::UnknownVar { id } => write!(f, "unknown var {}", id.0),
            EmitError::AddressTooWide { bits } => {
                write!(f, "storage address is {bits} bits; backends support at most 64")
            }
            EmitError::TypeUnsupported { ty } => write!(f, "unsupported type: {ty}"),
        }
    }
}

impl std::error::Error for EmitError {}

impl EmitError {
    pub fn unsupported(what: impl Into<String>, hint: impl Into<String>) -> Self {
        EmitError::Unsupported {
            what: what.into(),
            hint: Some(hint.into()),
        }
    }
}
