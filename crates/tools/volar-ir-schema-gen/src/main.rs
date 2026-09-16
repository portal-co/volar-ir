//! Generates committed, language-neutral Volar IR schema artifacts.
//!
//! The schema is deliberately data-only: target backends own how its types are
//! rendered, while this binary owns validation, ordering, and reproducibility.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process,
};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Schema {
    format: String,
    version: u32,
    #[serde(default)]
    roots: Vec<Root>,
    #[serde(default)]
    types: Vec<Type>,
    /// Definitions whose Rust representation is emitted by this generator.
    ///
    /// Keeping this separate from the portable inventory makes the transition
    /// incremental: a type enters `generated` only once its handwritten
    /// definition and rkyv derive have been removed.
    #[serde(default)]
    generated: Vec<GeneratedType>,
}

#[derive(Debug, Deserialize)]
struct Root {
    id: String,
    rust: String,
    typescript: String,
    text: TextProfile,
}

#[derive(Debug, Deserialize)]
struct TextProfile {
    kind: String,
    header: String,
}

#[derive(Debug, Deserialize)]
struct Type {
    rust: String,
    crate_name: String,
    kind: String,
    #[serde(default)]
    rkyv: bool,
}

#[derive(Debug, Deserialize)]
struct GeneratedType {
    id: String,
    #[serde(default)]
    doc: String,
    rust: GeneratedRust,
    typescript: String,
}

#[derive(Debug, Deserialize)]
struct GeneratedRust {
    output: String,
    name: String,
    kind: String,
    #[serde(default)]
    inner: String,
    #[serde(default)]
    variants: Vec<String>,
    #[serde(default)]
    fields: Vec<GeneratedField>,
    #[serde(default)]
    generics: Vec<GenericParam>,
    #[serde(default)]
    non_exhaustive: bool,
    #[serde(default = "default_true")]
    copy: bool,
    #[serde(default)]
    archived_ord: bool,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    derives: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GeneratedField {
    name: String,
    ty: String,
}

/// A deliberately small generic parameter model. It covers portable record
/// definitions without embedding Rust syntax in the data schema.
#[derive(Debug, Deserialize)]
struct GenericParam {
    name: String,
    #[serde(default)]
    bound: String,
    #[serde(default)]
    default: String,
}

fn default_true() -> bool {
    true
}

fn main() {
    let check = matches!(env::args().nth(1).as_deref(), Some("--check"));
    let root = workspace_root();
    let schema_path = root.join("schema/volar-ir.schema.json");
    let schema: Schema =
        serde_json::from_str(&fs::read_to_string(&schema_path).unwrap_or_else(|error| {
            fatal(&format!(
                "failed to read {}: {error}",
                schema_path.display()
            ))
        }))
        .unwrap_or_else(|error| fatal(&format!("failed to parse schema: {error}")));

    validate(&schema);
    let mut outputs = vec![
        (root.join("schema/GENERATED.md"), render_markdown(&schema)),
        (
            root.join("packages/volar-ir-text/src/generated/schema.ts"),
            render_typescript(&schema),
        ),
    ];
    for output in generated_outputs(&schema) {
        outputs.push(output);
    }

    let mut stale = false;
    for (path, contents) in outputs {
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current != contents {
            stale = true;
            if !check {
                fs::create_dir_all(path.parent().expect("output has parent")).unwrap_or_else(
                    |error| fatal(&format!("failed to create output directory: {error}")),
                );
                fs::write(&path, contents).unwrap_or_else(|error| {
                    fatal(&format!("failed to write {}: {error}", path.display()))
                });
            }
        }
    }
    if check && stale {
        fatal("generated schema artifacts are stale; run `cargo run -p volar-ir-schema-gen`");
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("generator is nested under crates/tools")
        .to_path_buf()
}

fn validate(schema: &Schema) {
    if schema.format != "volar-ir-schema" || schema.version != 1 {
        fatal("only volar-ir-schema v1 is supported");
    }
    let mut seen = std::collections::BTreeSet::new();
    for root in &schema.roots {
        if root.id.is_empty()
            || root.rust.is_empty()
            || root.typescript.is_empty()
            || root.text.kind.is_empty()
            || root.text.header.is_empty()
            || !seen.insert(&root.id)
        {
            fatal("each root needs unique id, Rust/TypeScript names, and a text profile");
        }
    }
    for ty in &schema.types {
        if ty.rust.is_empty() || ty.crate_name.is_empty() || ty.kind.is_empty() {
            fatal("each type needs a Rust path, owning crate, and kind");
        }
    }
    for ty in &schema.generated {
        if ty.id.is_empty()
            || ty.typescript.is_empty()
            || ty.rust.output.is_empty()
            || ty.rust.name.is_empty()
            || !valid_generated_type(&ty.rust)
        {
            fatal("each generated type must be a named scalar newtype with a supported inner type");
        }
        let mut generic_names = std::collections::BTreeSet::new();
        for generic in &ty.rust.generics {
            if !is_rust_identifier(&generic.name) || !generic_names.insert(&generic.name) {
                fatal("generated generic parameters need unique Rust identifier names");
            }
        }
    }
}

fn is_rust_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(character) if character == '_' || character.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn generated_outputs(schema: &Schema) -> Vec<(PathBuf, String)> {
    let root = workspace_root();
    let mut by_output: std::collections::BTreeMap<&str, Vec<&GeneratedType>> =
        std::collections::BTreeMap::new();
    for ty in &schema.generated {
        by_output.entry(&ty.rust.output).or_default().push(ty);
    }
    by_output
        .into_iter()
        .map(|(output, types)| (root.join(output), render_rust_types(&types)))
        .collect()
}

fn valid_generated_type(ty: &GeneratedRust) -> bool {
    match ty.kind.as_str() {
        "newtype" => archived_primitive(&ty.inner).is_some(),
        "unit-enum" => {
            !ty.variants.is_empty() && ty.variants.iter().all(|variant| !variant.is_empty())
        }
        "record" => {
            !ty.fields.is_empty()
                && ty
                    .fields
                    .iter()
                    .all(|field| !field.name.is_empty() && !field.ty.is_empty())
        }
        _ => false,
    }
}

fn archived_primitive(inner: &str) -> Option<(&'static str, &'static str)> {
    match inner {
        "u8" => Some(("u8", "self.0")),
        "u16" => Some(("rkyv::primitive::ArchivedU16", "self.0")),
        "u32" => Some(("rkyv::primitive::ArchivedU32", "self.0")),
        "u64" => Some(("rkyv::primitive::ArchivedU64", "self.0")),
        "u128" => Some(("rkyv::primitive::ArchivedU128", "self.0")),
        "usize" => Some(("rkyv::primitive::ArchivedUsize", "self.0 as _")),
        _ => None,
    }
}

fn archive_expression(inner: &str, access: &str) -> String {
    match inner {
        "u8" => access.to_owned(),
        "usize" => {
            let (archived, _) = archived_primitive(inner).expect("validated primitive");
            format!("{archived}::from_native(({access}) as _)")
        }
        _ => {
            let (archived, _) = archived_primitive(inner).expect("validated primitive");
            format!("{archived}::from_native({access})")
        }
    }
}

fn deserialize_expression(inner: &str, access: &str) -> String {
    match inner {
        "u8" => access.to_owned(),
        "usize" => format!("{access}.to_native() as usize"),
        _ => format!("{access}.to_native()"),
    }
}

fn render_rust_types(types: &[&GeneratedType]) -> String {
    let mut out = String::from(
        "// Generated by volar-ir-schema-gen; do not edit.\n//\n// This code deliberately implements rkyv's three traits directly and\n// generates byte validation for archived values. The archived layout matches\n// rkyv_derive's `#[repr(C)]` scalar-newtype layout, so pinned-rkyv binary\n// artifacts remain byte-compatible.\n\n",
    );
    for ty in types {
        match ty.rust.kind.as_str() {
            "newtype" => render_newtype(&mut out, ty),
            "unit-enum" => render_unit_enum(&mut out, ty),
            "record" => render_record(&mut out, ty),
            _ => unreachable!("validated generated type"),
        }
    }
    // Renderers separate definitions with a blank line. Do not leave that
    // separator as a spurious blank final line in generated Rust files.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn render_docs(out: &mut String, doc: &str) {
    for line in doc.lines() {
        writeln!(out, "/// {line}").unwrap();
    }
}

fn generic_declaration(generics: &[GenericParam], archive: bool, defaults: bool) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let parameters = generics
        .iter()
        .map(|generic| {
            let mut bounds = Vec::new();
            if !generic.bound.is_empty() {
                bounds.push(generic.bound.as_str());
            }
            if archive {
                bounds.push("rkyv::Archive");
            }
            let mut parameter = generic.name.clone();
            if !bounds.is_empty() {
                parameter.push_str(": ");
                parameter.push_str(&bounds.join(" + "));
            }
            if defaults && !generic.default.is_empty() {
                parameter.push_str(" = ");
                parameter.push_str(&generic.default);
            }
            parameter
        })
        .collect::<Vec<_>>();
    format!("<{}>", parameters.join(", "))
}

fn generic_arguments(generics: &[GenericParam]) -> String {
    if generics.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn render_newtype(out: &mut String, ty: &GeneratedType) {
    let rust = &ty.rust;
    let (archived, _) = archived_primitive(&rust.inner).expect("validated newtype");
    render_docs(out, &ty.doc);
    writeln!(out, "#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]\npub struct {}(pub {});\n", rust.name, rust.inner).unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\n#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, rkyv::bytecheck::CheckBytes)]\n#[bytecheck(crate = rkyv::bytecheck)]\n#[repr(C)]\npub struct Archived{}(pub {});\n", rust.name, archived).unwrap();
    writeln!(
        out,
        "#[cfg(feature = \"rkyv\")]\nunsafe impl rkyv::Portable for Archived{} {{}}\n",
        rust.name
    )
    .unwrap();
    let archived_value = archive_expression(&rust.inner, "self.0");
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl rkyv::Archive for {} {{\n    type Archived = Archived{};\n    type Resolver = ();\n\n    fn resolve(&self, (): Self::Resolver, out: rkyv::Place<Self::Archived>) {{\n        // SAFETY: the generated archived newtype is repr(C), contains exactly\n        // one initialized portable scalar, and has no padding.\n        unsafe {{\n            out.write_unchecked(Archived{}({}))\n        }}\n    }}\n}}\n", rust.name, rust.name, rust.name, archived_value).unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl<S: rkyv::rancor::Fallible + ?Sized> rkyv::Serialize<S> for {} {{\n    fn serialize(&self, _: &mut S) -> Result<Self::Resolver, S::Error> {{\n        Ok(())\n    }}\n}}\n", rust.name).unwrap();
    let native = deserialize_expression(&rust.inner, "self.0");
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl<D: rkyv::rancor::Fallible + ?Sized> rkyv::Deserialize<{}, D>\n    for Archived{}\n{{\n    fn deserialize(&self, _: &mut D) -> Result<{}, D::Error> {{\n        Ok({}({}))\n    }}\n}}\n", rust.name, rust.name, rust.name, rust.name, native).unwrap();
}

fn render_unit_enum(out: &mut String, ty: &GeneratedType) {
    let rust = &ty.rust;
    render_docs(out, &ty.doc);
    writeln!(
        out,
        "#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]"
    )
    .unwrap();
    if rust.non_exhaustive {
        writeln!(out, "#[non_exhaustive]").unwrap();
    }
    writeln!(out, "pub enum {} {{", rust.name).unwrap();
    for variant in &rust.variants {
        writeln!(out, "    {variant},").unwrap();
    }
    writeln!(out, "}}\n").unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\n#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, rkyv::bytecheck::CheckBytes)]\n#[bytecheck(crate = rkyv::bytecheck)]\n#[repr(u8)]\npub enum Archived{} {{", rust.name).unwrap();
    for variant in &rust.variants {
        writeln!(out, "    {variant},").unwrap();
    }
    writeln!(
        out,
        "}}\n\n#[cfg(feature = \"rkyv\")]\n#[derive(Clone, Copy, Debug)]\npub enum {}Resolver {{",
        rust.name
    )
    .unwrap();
    for variant in &rust.variants {
        writeln!(out, "    {variant},").unwrap();
    }
    writeln!(
        out,
        "}}\n\n#[cfg(feature = \"rkyv\")]\nunsafe impl rkyv::Portable for Archived{} {{}}\n",
        rust.name
    )
    .unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl rkyv::Archive for {} {{\n    type Archived = Archived{};\n    type Resolver = {}Resolver;\n\n    fn resolve(&self, resolver: Self::Resolver, out: rkyv::Place<Self::Archived>) {{\n        let archived = match resolver {{", rust.name, rust.name, rust.name).unwrap();
    for variant in &rust.variants {
        writeln!(
            out,
            "            {}Resolver::{variant} => Archived{}::{variant},",
            rust.name, rust.name
        )
        .unwrap();
    }
    writeln!(out, "        }};\n        // SAFETY: `archived` is a fully initialized repr(u8) discriminant.\n        unsafe {{ out.write_unchecked(archived) }}\n    }}\n}}\n").unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl<S: rkyv::rancor::Fallible + ?Sized> rkyv::Serialize<S> for {} {{\n    fn serialize(&self, _: &mut S) -> Result<Self::Resolver, S::Error> {{\n        Ok(match self {{", rust.name).unwrap();
    for variant in &rust.variants {
        writeln!(
            out,
            "            {}::{variant} => {}Resolver::{variant},",
            rust.name, rust.name
        )
        .unwrap();
    }
    writeln!(out, "        }})\n    }}\n}}\n").unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl<D: rkyv::rancor::Fallible + ?Sized> rkyv::Deserialize<{}, D>\n    for Archived{}\n{{\n    fn deserialize(&self, _: &mut D) -> Result<{}, D::Error> {{\n        Ok(match self {{", rust.name, rust.name, rust.name).unwrap();
    for variant in &rust.variants {
        writeln!(
            out,
            "            Archived{}::{variant} => {}::{variant},",
            rust.name, rust.name
        )
        .unwrap();
    }
    writeln!(out, "        }})\n    }}\n}}\n").unwrap();
}

fn render_record(out: &mut String, ty: &GeneratedType) {
    let rust = &ty.rust;
    let native_generics = generic_declaration(&rust.generics, false, true);
    let archived_generics = generic_declaration(&rust.generics, true, true);
    let impl_generics = generic_declaration(&rust.generics, true, false);
    let generic_arguments = generic_arguments(&rust.generics);
    render_docs(out, &ty.doc);
    let native_derives = if rust.copy {
        "Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug"
    } else {
        "Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug"
    };
    let native_derives = if rust.derives.is_empty() {
        native_derives.to_owned()
    } else {
        rust.derives.join(", ")
    };
    let native_derives = if rust.default {
        format!("{native_derives}, Default")
    } else {
        native_derives
    };
    writeln!(
        out,
        "#[derive({native_derives})]\npub struct {}{} {{",
        rust.name, native_generics
    )
    .unwrap();
    for field in &rust.fields {
        writeln!(out, "    pub {}: {},", field.name, field.ty).unwrap();
    }
    writeln!(out, "}}\n").unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]").unwrap();
    if rust.archived_ord {
        writeln!(out, "#[derive(PartialEq, Eq, PartialOrd, Ord)]").unwrap();
    }
    writeln!(out, "#[derive(rkyv::bytecheck::CheckBytes)]").unwrap();
    writeln!(out, "#[bytecheck(crate = rkyv::bytecheck)]").unwrap();
    writeln!(
        out,
        "#[repr(C)]\npub struct Archived{}{} {{",
        rust.name, archived_generics
    )
    .unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "    pub {}: <{} as rkyv::Archive>::Archived,",
            field.name, field.ty
        )
        .unwrap();
    }
    writeln!(
        out,
        "}}\n\n#[cfg(feature = \"rkyv\")]\n#[allow(dead_code)]\npub struct {}Resolver{} {{",
        rust.name, archived_generics
    )
    .unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "    {}: <{} as rkyv::Archive>::Resolver,",
            field.name, field.ty
        )
        .unwrap();
    }
    writeln!(
        out,
        "}}\n\n#[cfg(feature = \"rkyv\")]\nunsafe impl{} rkyv::Portable for Archived{}{}\nwhere",
        impl_generics, rust.name, generic_arguments
    )
    .unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "    <{} as rkyv::Archive>::Archived: rkyv::Portable,",
            field.ty
        )
        .unwrap();
    }
    writeln!(out, "{{}}\n").unwrap();
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl{} rkyv::Archive for {}{} {{\n    type Archived = Archived{}{};\n    type Resolver = {}Resolver{};\n\n    fn resolve(&self, resolver: Self::Resolver, out: rkyv::Place<Self::Archived>) {{", impl_generics, rust.name, generic_arguments, rust.name, generic_arguments, rust.name, generic_arguments).unwrap();
    for field in &rust.fields {
        writeln!(out, "        let field_ptr = unsafe {{ ::core::ptr::addr_of_mut!((*out.ptr()).{}) }};\n        let field_out = unsafe {{ rkyv::Place::from_field_unchecked(out, field_ptr) }};\n        rkyv::Archive::resolve(&self.{}, resolver.{}, field_out);", field.name, field.name, field.name).unwrap();
    }
    writeln!(out, "    }}\n}}\n").unwrap();
    let serialize_generics = if impl_generics.is_empty() {
        "<S: rkyv::rancor::Fallible + ?Sized>".to_owned()
    } else {
        format!(
            "<{}, S: rkyv::rancor::Fallible + ?Sized>",
            impl_generics.trim_start_matches('<').trim_end_matches('>')
        )
    };
    writeln!(
        out,
        "#[cfg(feature = \"rkyv\")]\nimpl{} rkyv::Serialize<S> for {}{}\nwhere",
        serialize_generics, rust.name, generic_arguments
    )
    .unwrap();
    for field in &rust.fields {
        writeln!(out, "    {}: rkyv::Serialize<S>,", field.ty).unwrap();
    }
    writeln!(out, "{{\n    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {{\n        Ok({}Resolver {{", rust.name).unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "            {}: rkyv::Serialize::serialize(&self.{}, serializer)?,",
            field.name, field.name
        )
        .unwrap();
    }
    writeln!(out, "        }})\n    }}\n}}\n").unwrap();
    let deserialize_generics = if impl_generics.is_empty() {
        "<D: rkyv::rancor::Fallible + ?Sized>".to_owned()
    } else {
        format!(
            "<{}, D: rkyv::rancor::Fallible + ?Sized>",
            impl_generics.trim_start_matches('<').trim_end_matches('>')
        )
    };
    writeln!(out, "#[cfg(feature = \"rkyv\")]\nimpl{} rkyv::Deserialize<{}{}, D>\n    for Archived{}{}\nwhere", deserialize_generics, rust.name, generic_arguments, rust.name, generic_arguments).unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "    <{} as rkyv::Archive>::Archived: rkyv::Deserialize<{}, D>,",
            field.ty, field.ty
        )
        .unwrap();
    }
    writeln!(out, "{{\n    fn deserialize(&self, deserializer: &mut D) -> Result<{}{}, D::Error> {{\n        Ok({} {{", rust.name, generic_arguments, rust.name).unwrap();
    for field in &rust.fields {
        writeln!(
            out,
            "            {}: rkyv::Deserialize::deserialize(&self.{}, deserializer)?,",
            field.name, field.name
        )
        .unwrap();
    }
    writeln!(out, "        }})\n    }}\n}}\n").unwrap();
}

fn render_markdown(schema: &Schema) -> String {
    let mut out = String::from(
        "<!-- Generated by volar-ir-schema-gen; do not edit. -->\n\n# Volar IR portable schema\n\n",
    );
    writeln!(out, "Schema: `{}` v{}.", schema.format, schema.version).unwrap();
    out.push_str("\n## Text roots\n\n| Root | Rust | Text profile | Header |\n|---|---|---|---|\n");
    for root in &schema.roots {
        writeln!(
            out,
            "| `{}` | `{}` | `{}` | `{}` |",
            root.id, root.rust, root.text.kind, root.text.header
        )
        .unwrap();
    }
    out.push_str(
        "\n## Portable Rust data\n\n| Type | Crate | Shape | rkyv binary |\n|---|---|---|---|\n",
    );
    for ty in &schema.types {
        writeln!(
            out,
            "| `{}` | `{}` | `{}` | {} |",
            ty.rust,
            ty.crate_name,
            ty.kind,
            if ty.rkyv { "yes" } else { "no" }
        )
        .unwrap();
    }
    if !schema.generated.is_empty() {
        out.push_str("\n## Generated definitions\n\n| Schema ID | Rust definition | TypeScript definition |\n|---|---|---|\n");
        for ty in &schema.generated {
            writeln!(
                out,
                "| `{}` | `{}` | `{}` |",
                ty.id, ty.rust.name, ty.typescript
            )
            .unwrap();
        }
    }
    out
}

fn render_typescript(schema: &Schema) -> String {
    let mut out = String::from("// Generated by volar-ir-schema-gen; do not edit.\n\n");
    out.push_str("export type PortableRootId =\n");
    for root in &schema.roots {
        writeln!(out, "  | \"{}\"", root.id).unwrap();
    }
    out.push_str(";\n\nexport interface RootMetadata {\n  readonly rust: string;\n  readonly typescript: string;\n  readonly profile: string;\n  readonly header: string;\n}\n\nexport const ROOTS: Readonly<Record<PortableRootId, RootMetadata>> = {\n");
    for root in &schema.roots {
        writeln!(
            out,
            "  \"{}\": {{ rust: \"{}\", typescript: \"{}\", profile: \"{}\", header: \"{}\" }},",
            root.id, root.rust, root.typescript, root.text.kind, root.text.header
        )
        .unwrap();
    }
    out.push_str("};\n");
    if !schema.generated.is_empty() {
        out.push_str("\n// Generated language-neutral data definitions. Integers are bigint so browser\n// and Node callers preserve the full portable integer domain.\n");
        for ty in &schema.generated {
            match ty.rust.kind.as_str() {
                "newtype" => writeln!(out, "export type {} = bigint;", ty.typescript).unwrap(),
                "unit-enum" => {
                    writeln!(out, "export type {} =", ty.typescript).unwrap();
                    for variant in &ty.rust.variants {
                        writeln!(out, "  | \"{variant}\"").unwrap();
                    }
                    out.push_str(";\n");
                }
                "record" => {
                    let generics = if ty.rust.generics.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "<{}>",
                            ty.rust
                                .generics
                                .iter()
                                .map(|generic| format!("{} = unknown", generic.name))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    writeln!(out, "export interface {}{} {{", ty.typescript, generics).unwrap();
                    for field in &ty.rust.fields {
                        writeln!(
                            out,
                            "  readonly {}: {};",
                            field.name,
                            typescript_field_type(schema, &field.ty, &ty.rust.generics)
                        )
                        .unwrap();
                    }
                    out.push_str("}\n");
                }
                _ => unreachable!("validated generated type"),
            }
        }
    }
    out
}

fn typescript_field_type(schema: &Schema, rust_type: &str, generics: &[GenericParam]) -> String {
    if let Some(element) = rust_type
        .strip_prefix("alloc::vec::Vec<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return format!(
            "ReadonlyArray<{}>",
            typescript_field_type(schema, element, generics)
        );
    }
    if generics.iter().any(|generic| generic.name == rust_type) {
        return rust_type.to_owned();
    }
    if let Some((base, arguments)) = split_generic_type(rust_type) {
        if let Some(ty) = schema.generated.iter().find(|ty| {
            ty.rust.name == base || base.strip_suffix(&format!("::{}", ty.rust.name)).is_some()
        }) {
            return format!(
                "{}<{}>",
                ty.typescript,
                arguments
                    .iter()
                    .map(|argument| typescript_field_type(schema, argument, generics))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    match rust_type {
        "bool" => "boolean".to_owned(),
        "alloc::string::String" => "string".to_owned(),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => "bigint".to_owned(),
        _ => schema
            .generated
            .iter()
            .find(|ty| {
                ty.rust.name == rust_type
                    || rust_type
                        .strip_suffix(&format!("::{}", ty.rust.name))
                        .is_some()
            })
            .map(|ty| ty.typescript.clone())
            // An unmodelled nested type remains explicit rather than silently
            // becoming `any`; its definition is added to the schema before a
            // typed consumer relies on it.
            .unwrap_or_else(|| "unknown".to_owned()),
    }
}

fn split_generic_type(value: &str) -> Option<(&str, Vec<&str>)> {
    let start = value.find('<')?;
    if !value.ends_with('>') {
        return None;
    }
    let base = &value[..start];
    let mut depth = 0_usize;
    let mut argument_start = start + 1;
    let mut arguments = Vec::new();
    for (index, character) in value
        .char_indices()
        .skip_while(|(index, _)| *index <= start)
    {
        match character {
            '<' => depth += 1,
            '>' if depth == 0 => {
                if index + 1 != value.len() {
                    return None;
                }
                arguments.push(value[argument_start..index].trim());
                return Some((base, arguments));
            }
            '>' => depth -= 1,
            ',' if depth == 0 => {
                arguments.push(value[argument_start..index].trim());
                argument_start = index + 1;
            }
            _ => {}
        }
    }
    None
}

fn fatal(message: &str) -> ! {
    eprintln!("volar-ir-schema-gen: {message}");
    process::exit(1)
}
