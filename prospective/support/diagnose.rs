//! Read-only structured observation diagnostics for the combination prototype.
//!
//! This is experiment-local support, not Dropwise API. It reuses
//! [`serde::Serialize`] as the traversal protocol and records the Serde data
//! model in a small value tree. It does not decide a verdict: the original
//! `PartialEq` remains the oracle.
//!
//! Why not `serde_json::Value` or `serde_value::Value`?
//!
//! * JSON maps `None` to `null`, so `Some(())` and `None` are not preserved,
//!   and it needs its own feature for 128-bit integers.
//! * `serde_value` 0.7 stores a unit variant as a bare string, a newtype
//!   variant as a map keyed by the variant name, and has no `i128`/`u128`
//!   nodes. Enum names and the newtype wrapper are lost.
//!
//! Both are useful general-purpose values, but they drop distinctions this
//! diagnostic wants to show. The serializer below is deliberately small: it
//! keeps Option/Some/None, Result/variants, newtypes, integers (including
//! 128-bit), sequence order and struct field names. It only sees what a type's
//! `Serialize` implementation emits; a transparent, custom or untagged
//! serializer can still hide information. That limitation is documented, not
//! papered over: a serialization error is returned, never turned into an empty
//! "equal" observation.
#![allow(dead_code)]

use std::fmt::{self, Display};

use serde::Serialize;
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant, Serializer,
};

/// A lossless-as-possible record of what one `Serialize` implementation emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Unit,
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    F32(u32),
    F64(u64),
    Char(char),
    String(String),
    Bytes(Vec<u8>),
    None,
    Some(Box<Value>),
    UnitStruct {
        name: &'static str,
    },
    NewtypeStruct {
        name: &'static str,
        value: Box<Value>,
    },
    UnitVariant {
        enum_name: &'static str,
        variant: &'static str,
    },
    NewtypeVariant {
        enum_name: &'static str,
        variant: &'static str,
        value: Box<Value>,
    },
    Seq(Vec<Value>),
    Tuple(Vec<Value>),
    TupleStruct {
        name: &'static str,
        values: Vec<Value>,
    },
    TupleVariant {
        enum_name: &'static str,
        variant: &'static str,
        values: Vec<Value>,
    },
    Map(Vec<(Value, Value)>),
    Struct {
        name: &'static str,
        fields: Vec<(&'static str, Value)>,
    },
    StructVariant {
        enum_name: &'static str,
        variant: &'static str,
        fields: Vec<(&'static str, Value)>,
    },
}

/// A serialization problem. Callers must report it; it never means "equal".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticError(String);

impl DiagnosticError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl Display for DiagnosticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DiagnosticError {}

impl ser::Error for DiagnosticError {
    fn custom<T: Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// One field-path difference between an observed and a reference value tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDifference {
    /// A path rooted at `$`, e.g. `$.own.Ok` or `$.items[2]`.
    pub path: String,
    /// The actual observation side. Never replaced by an expected input.
    pub observed: String,
    /// The calibration/reference side.
    pub reference: String,
}

/// Which declared identity an observed string matches.
///
/// This is neutral bookkeeping, not an oracle. Equality with the target does
/// not establish causal provenance, and matching the target is not
/// automatically a violation: a completed target effect can be legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityMatch {
    MatchesTarget,
    MatchesSuccessor,
    MatchesBoth,
    Unmatched,
}

impl IdentityMatch {
    /// Compare one raw observed string with the target/successor labels.
    /// `None` means that side has no business identity for this case.
    pub fn classify(
        observed: &str,
        target: Option<&str>,
        successor: Option<&str>,
    ) -> IdentityMatch {
        let matches_target = target == Some(observed);
        let matches_successor = successor == Some(observed);
        match (matches_target, matches_successor) {
            (true, true) => IdentityMatch::MatchesBoth,
            (true, false) => IdentityMatch::MatchesTarget,
            (false, true) => IdentityMatch::MatchesSuccessor,
            (false, false) => IdentityMatch::Unmatched,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            IdentityMatch::MatchesTarget => "MatchesTarget",
            IdentityMatch::MatchesSuccessor => "MatchesSuccessor",
            IdentityMatch::MatchesBoth => "MatchesBoth",
            IdentityMatch::Unmatched => "Unmatched",
        }
    }
}

/// One adapter-declared binding of an observed field to the case's identities.
/// The raw observation is kept next to the target/successor labels and the
/// neutral match above; matching alone does not change the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityBinding {
    pub field: String,
    pub observed: String,
    pub target: Option<String>,
    pub successor: Option<String>,
    pub matched: IdentityMatch,
}

impl IdentityBinding {
    pub fn new(
        field: impl Into<String>,
        observed: impl Into<String>,
        target: Option<&str>,
        successor: Option<&str>,
    ) -> Self {
        let observed = observed.into();
        let matched = IdentityMatch::classify(&observed, target, successor);
        Self {
            field: field.into(),
            observed,
            target: target.map(str::to_string),
            successor: successor.map(str::to_string),
            matched,
        }
    }
}

/// Serialize one value into the diagnostic tree. A custom serializer that
/// returns an error is reported as an error, never as an empty tree.
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, DiagnosticError> {
    value.serialize(ValueSerializer)
}

/// Serialize both sides and diff them. The first value is the observation; the
/// second is the calibration/reference. Only emitted fields can be compared.
pub fn diff_observations<O: Serialize + ?Sized, R: Serialize + ?Sized>(
    observed: &O,
    reference: &R,
) -> Result<Vec<FieldDifference>, DiagnosticError> {
    let observed = to_value(observed)
        .map_err(|err| DiagnosticError::new(format!("observed serialization failed: {err}")))?;
    let reference = to_value(reference)
        .map_err(|err| DiagnosticError::new(format!("reference serialization failed: {err}")))?;
    Ok(diff_values(&observed, &reference))
}

/// Diff two already-serialized trees. Exposed so a caller can serialize once
/// per observation and compare it with several calibration samples.
pub fn diff_values(observed: &Value, reference: &Value) -> Vec<FieldDifference> {
    let mut differences = Vec::new();
    walk(&mut Vec::new(), observed, reference, &mut differences);
    differences
}

fn walk(
    path: &mut Vec<String>,
    observed: &Value,
    reference: &Value,
    differences: &mut Vec<FieldDifference>,
) {
    match (observed, reference) {
        (Value::None, Value::None) => {}
        (Value::Some(observed), Value::Some(reference)) => {
            walk(path, observed, reference, differences);
        }
        (Value::None, reference) => record(path, "None", render(reference), differences),
        (observed, Value::None) => record(path, render(observed), "None", differences),
        (
            Value::NewtypeStruct {
                name: observed_name,
                value: observed,
            },
            Value::NewtypeStruct {
                name: reference_name,
                value: reference,
            },
        ) if observed_name == reference_name => {
            walk(path, observed, reference, differences);
        }
        (Value::UnitStruct { name: observed }, Value::UnitStruct { name: reference })
            if observed == reference => {}
        (
            Value::UnitVariant {
                enum_name: observed_enum,
                variant: observed_variant,
            },
            Value::UnitVariant {
                enum_name: reference_enum,
                variant: reference_variant,
            },
        ) if observed_enum == reference_enum && observed_variant == reference_variant => {}
        (
            Value::NewtypeVariant {
                enum_name: observed_enum,
                variant: observed_variant,
                value: observed,
            },
            Value::NewtypeVariant {
                enum_name: reference_enum,
                variant: reference_variant,
                value: reference,
            },
        ) if observed_enum == reference_enum && observed_variant == reference_variant => {
            path.push(format!(".{observed_variant}"));
            walk(path, observed, reference, differences);
            path.pop();
        }
        (
            Value::TupleVariant {
                enum_name: observed_enum,
                variant: observed_variant,
                values: observed,
            },
            Value::TupleVariant {
                enum_name: reference_enum,
                variant: reference_variant,
                values: reference,
            },
        ) if observed_enum == reference_enum && observed_variant == reference_variant => {
            path.push(format!(".{observed_variant}"));
            walk_sequence(path, observed, reference, differences);
            path.pop();
        }
        (
            Value::StructVariant {
                enum_name: observed_enum,
                variant: observed_variant,
                fields: observed,
            },
            Value::StructVariant {
                enum_name: reference_enum,
                variant: reference_variant,
                fields: reference,
            },
        ) if observed_enum == reference_enum && observed_variant == reference_variant => {
            path.push(format!(".{observed_variant}"));
            walk_fields(path, observed, reference, differences);
            path.pop();
        }
        (Value::Seq(observed), Value::Seq(reference))
        | (Value::Tuple(observed), Value::Tuple(reference)) => {
            walk_sequence(path, observed, reference, differences);
        }
        (
            Value::TupleStruct {
                name: observed_name,
                values: observed,
            },
            Value::TupleStruct {
                name: reference_name,
                values: reference,
            },
        ) if observed_name == reference_name => {
            walk_sequence(path, observed, reference, differences);
        }
        (Value::Map(observed), Value::Map(reference)) => {
            walk_map(path, observed, reference, differences);
        }
        (
            Value::Struct {
                name: observed_name,
                fields: observed,
            },
            Value::Struct {
                name: reference_name,
                fields: reference,
            },
        ) if observed_name == reference_name => {
            walk_fields(path, observed, reference, differences);
        }
        (observed, reference) if same_primitive_shape(observed, reference) => {
            if observed != reference {
                record(path, render(observed), render(reference), differences);
            }
        }
        (observed, reference) => {
            record(path, render(observed), render(reference), differences);
        }
    }
}

fn same_primitive_shape(observed: &Value, reference: &Value) -> bool {
    matches!(
        (observed, reference),
        (Value::Unit, Value::Unit)
            | (Value::Bool(_), Value::Bool(_))
            | (Value::I8(_), Value::I8(_))
            | (Value::I16(_), Value::I16(_))
            | (Value::I32(_), Value::I32(_))
            | (Value::I64(_), Value::I64(_))
            | (Value::I128(_), Value::I128(_))
            | (Value::U8(_), Value::U8(_))
            | (Value::U16(_), Value::U16(_))
            | (Value::U32(_), Value::U32(_))
            | (Value::U64(_), Value::U64(_))
            | (Value::U128(_), Value::U128(_))
            | (Value::F32(_), Value::F32(_))
            | (Value::F64(_), Value::F64(_))
            | (Value::Char(_), Value::Char(_))
            | (Value::String(_), Value::String(_))
            | (Value::Bytes(_), Value::Bytes(_))
    )
}

fn walk_sequence(
    path: &mut Vec<String>,
    observed: &[Value],
    reference: &[Value],
    differences: &mut Vec<FieldDifference>,
) {
    if observed.len() != reference.len() {
        record(
            path,
            format!("len={}", observed.len()),
            format!("len={}", reference.len()),
            differences,
        );
    }
    for index in 0..observed.len().min(reference.len()) {
        path.push(format!("[{index}]"));
        walk(path, &observed[index], &reference[index], differences);
        path.pop();
    }
    for (index, value) in observed.iter().enumerate().skip(reference.len()) {
        path.push(format!("[{index}]"));
        record(path, render(value), "<missing>", differences);
        path.pop();
    }
    for (index, value) in reference.iter().enumerate().skip(observed.len()) {
        path.push(format!("[{index}]"));
        record(path, "<missing>", render(value), differences);
        path.pop();
    }
}

fn walk_fields(
    path: &mut Vec<String>,
    observed: &[(&'static str, Value)],
    reference: &[(&'static str, Value)],
    differences: &mut Vec<FieldDifference>,
) {
    for (name, observed_value) in observed {
        path.push(format!(".{name}"));
        match reference
            .iter()
            .find(|(reference_name, _)| reference_name == name)
        {
            Some((_, reference_value)) => walk(path, observed_value, reference_value, differences),
            None => record(path, render(observed_value), "<missing>", differences),
        }
        path.pop();
    }
    for (name, reference_value) in reference {
        if observed
            .iter()
            .all(|(observed_name, _)| observed_name != name)
        {
            path.push(format!(".{name}"));
            record(path, "<missing>", render(reference_value), differences);
            path.pop();
        }
    }
}

fn walk_map(
    path: &mut Vec<String>,
    observed: &[(Value, Value)],
    reference: &[(Value, Value)],
    differences: &mut Vec<FieldDifference>,
) {
    let mut matched = vec![false; reference.len()];
    for (key, observed_value) in observed {
        path.push(format!("[{}]", render(key)));
        match reference
            .iter()
            .enumerate()
            .find(|(index, (reference_key, _))| !matched[*index] && reference_key == key)
        {
            Some((index, (_, reference_value))) => {
                matched[index] = true;
                walk(path, observed_value, reference_value, differences);
            }
            None => record(path, render(observed_value), "<missing>", differences),
        }
        path.pop();
    }
    for (index, (key, reference_value)) in reference.iter().enumerate() {
        if !matched[index] {
            path.push(format!("[{}]", render(key)));
            record(path, "<missing>", render(reference_value), differences);
            path.pop();
        }
    }
}

fn record(
    path: &[String],
    observed: impl Into<String>,
    reference: impl Into<String>,
    differences: &mut Vec<FieldDifference>,
) {
    differences.push(FieldDifference {
        path: field_path(path),
        observed: observed.into(),
        reference: reference.into(),
    });
}

fn field_path(path: &[String]) -> String {
    if path.is_empty() {
        "$".to_string()
    } else {
        format!("${}", path.concat())
    }
}

/// Render a value tree for a diagnostic line. This is not `Debug` output and
/// is not parsed back; it is only a lossless-as-possible display of `Value`.
pub fn render(value: &Value) -> String {
    match value {
        Value::Unit => "()".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::I8(value) => value.to_string(),
        Value::I16(value) => value.to_string(),
        Value::I32(value) => value.to_string(),
        Value::I64(value) => value.to_string(),
        Value::I128(value) => value.to_string(),
        Value::U8(value) => value.to_string(),
        Value::U16(value) => value.to_string(),
        Value::U32(value) => value.to_string(),
        Value::U64(value) => value.to_string(),
        Value::U128(value) => value.to_string(),
        Value::F32(bits) => render_f32(*bits),
        Value::F64(bits) => render_f64(*bits),
        Value::Char(value) => format!("{value:?}"),
        Value::String(value) => format!("{value:?}"),
        Value::Bytes(value) => format!("bytes({value:?})"),
        Value::None => "None".to_string(),
        Value::Some(value) => format!("Some({})", render(value)),
        Value::UnitStruct { name } => (*name).to_string(),
        Value::NewtypeStruct { name, value } => format!("{name}({})", render(value)),
        Value::UnitVariant { enum_name, variant } => variant_name(enum_name, variant),
        Value::NewtypeVariant {
            enum_name,
            variant,
            value,
        } => {
            format!("{}({})", variant_name(enum_name, variant), render(value))
        }
        Value::Seq(values) => format!("[{}]", render_all(values)),
        Value::Tuple(values) => format!("({})", render_all(values)),
        Value::TupleStruct { name, values } => format!("{name}({})", render_all(values)),
        Value::TupleVariant {
            enum_name,
            variant,
            values,
        } => {
            format!(
                "{}({})",
                variant_name(enum_name, variant),
                render_all(values)
            )
        }
        Value::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{}: {}", render(key), render(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Struct { name, fields } => {
            format!("{name} {{{}}}", render_fields(fields))
        }
        Value::StructVariant {
            enum_name,
            variant,
            fields,
        } => format!(
            "{} {{{}}}",
            variant_name(enum_name, variant),
            render_fields(fields)
        ),
    }
}

fn render_f32(bits: u32) -> String {
    let value = f32::from_bits(bits);
    if value.is_nan() {
        format!("NaN(bits={bits:#010x})")
    } else {
        format!("{value:?}")
    }
}

fn render_f64(bits: u64) -> String {
    let value = f64::from_bits(bits);
    if value.is_nan() {
        format!("NaN(bits={bits:#018x})")
    } else {
        format!("{value:?}")
    }
}

fn variant_name(enum_name: &str, variant: &str) -> String {
    if enum_name.is_empty() {
        variant.to_string()
    } else {
        format!("{enum_name}::{variant}")
    }
}

fn render_all(values: &[Value]) -> String {
    values.iter().map(render).collect::<Vec<_>>().join(", ")
}

fn render_fields(fields: &[(&'static str, Value)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{name}: {}", render(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

struct ValueSerializer;

impl Serializer for ValueSerializer {
    type Ok = Value;
    type Error = DiagnosticError;
    type SerializeSeq = Compound;
    type SerializeTuple = Compound;
    type SerializeTupleStruct = Compound;
    type SerializeTupleVariant = Compound;
    type SerializeMap = Compound;
    type SerializeStruct = Compound;
    type SerializeStructVariant = Compound;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Bool(value))
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        Ok(Value::I8(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        Ok(Value::I16(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        Ok(Value::I32(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::I64(value))
    }

    fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
        Ok(Value::I128(value))
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        Ok(Value::U8(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        Ok(Value::U16(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        Ok(Value::U32(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::U64(value))
    }

    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        Ok(Value::U128(value))
    }

    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        Ok(Value::F32(value.to_bits()))
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::F64(value.to_bits()))
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Char(value))
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        Ok(Value::String(value.to_string()))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Bytes(value.to_vec()))
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::None)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Some(Box::new(value.serialize(ValueSerializer)?)))
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Unit)
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(Value::UnitStruct { name })
    }

    fn serialize_unit_variant(
        self,
        enum_name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(Value::UnitVariant { enum_name, variant })
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(Value::NewtypeStruct {
            name,
            value: Box::new(value.serialize(ValueSerializer)?),
        })
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        enum_name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(Value::NewtypeVariant {
            enum_name,
            variant,
            value: Box::new(value.serialize(ValueSerializer)?),
        })
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(Compound::seq(len))
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(Compound::seq(Some(len)))
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(Compound::tuple_struct(name, len))
    }

    fn serialize_tuple_variant(
        self,
        enum_name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(Compound::tuple_variant(enum_name, variant, len))
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(Compound::map(len))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(Compound::struct_(name, len))
    }

    fn serialize_struct_variant(
        self,
        enum_name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(Compound::struct_variant(enum_name, variant, len))
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

enum CompoundKind {
    Seq(Vec<Value>),
    TupleStruct {
        name: &'static str,
        values: Vec<Value>,
    },
    TupleVariant {
        enum_name: &'static str,
        variant: &'static str,
        values: Vec<Value>,
    },
    Map {
        entries: Vec<(Value, Value)>,
        key: Option<Value>,
    },
    Struct {
        name: &'static str,
        fields: Vec<(&'static str, Value)>,
    },
    StructVariant {
        enum_name: &'static str,
        variant: &'static str,
        fields: Vec<(&'static str, Value)>,
    },
}

struct Compound {
    kind: CompoundKind,
}

impl Compound {
    fn seq(len: Option<usize>) -> Self {
        Self {
            kind: CompoundKind::Seq(Vec::with_capacity(len.unwrap_or(0))),
        }
    }

    fn tuple_struct(name: &'static str, len: usize) -> Self {
        Self {
            kind: CompoundKind::TupleStruct {
                name,
                values: Vec::with_capacity(len),
            },
        }
    }

    fn tuple_variant(enum_name: &'static str, variant: &'static str, len: usize) -> Self {
        Self {
            kind: CompoundKind::TupleVariant {
                enum_name,
                variant,
                values: Vec::with_capacity(len),
            },
        }
    }

    fn map(len: Option<usize>) -> Self {
        Self {
            kind: CompoundKind::Map {
                entries: Vec::with_capacity(len.unwrap_or(0)),
                key: None,
            },
        }
    }

    fn struct_(name: &'static str, len: usize) -> Self {
        Self {
            kind: CompoundKind::Struct {
                name,
                fields: Vec::with_capacity(len),
            },
        }
    }

    fn struct_variant(enum_name: &'static str, variant: &'static str, len: usize) -> Self {
        Self {
            kind: CompoundKind::StructVariant {
                enum_name,
                variant,
                fields: Vec::with_capacity(len),
            },
        }
    }

    fn push_value(&mut self, value: Value) -> Result<(), DiagnosticError> {
        match &mut self.kind {
            CompoundKind::Seq(values)
            | CompoundKind::TupleStruct { values, .. }
            | CompoundKind::TupleVariant { values, .. } => {
                values.push(value);
                Ok(())
            }
            _ => Err(DiagnosticError::new(
                "serializer pushed a value into a non-sequence compound",
            )),
        }
    }

    fn push_field(&mut self, name: &'static str, value: Value) -> Result<(), DiagnosticError> {
        match &mut self.kind {
            CompoundKind::Struct { fields, .. } | CompoundKind::StructVariant { fields, .. } => {
                fields.push((name, value));
                Ok(())
            }
            _ => Err(DiagnosticError::new(
                "serializer pushed a field into a non-struct compound",
            )),
        }
    }
}

impl SerializeSeq for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push_value(value.serialize(ValueSerializer)?)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::Seq(values) => Ok(Value::Seq(values)),
            _ => Err(DiagnosticError::new(
                "sequence serializer ended with another kind",
            )),
        }
    }
}

impl SerializeTuple for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push_value(value.serialize(ValueSerializer)?)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::Seq(values) => Ok(Value::Tuple(values)),
            _ => Err(DiagnosticError::new(
                "tuple serializer ended with another kind",
            )),
        }
    }
}

impl SerializeTupleStruct for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push_value(value.serialize(ValueSerializer)?)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::TupleStruct { name, values } => Ok(Value::TupleStruct { name, values }),
            _ => Err(DiagnosticError::new(
                "tuple-struct serializer ended with another kind",
            )),
        }
    }
}

impl SerializeTupleVariant for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push_value(value.serialize(ValueSerializer)?)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::TupleVariant {
                enum_name,
                variant,
                values,
            } => Ok(Value::TupleVariant {
                enum_name,
                variant,
                values,
            }),
            _ => Err(DiagnosticError::new(
                "tuple-variant serializer ended with another kind",
            )),
        }
    }
}

impl SerializeMap for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        match &mut self.kind {
            CompoundKind::Map { key: slot, .. } => {
                if slot.replace(key.serialize(ValueSerializer)?).is_some() {
                    return Err(DiagnosticError::new(
                        "serializer received two map keys without a value",
                    ));
                }
                Ok(())
            }
            _ => Err(DiagnosticError::new(
                "map serializer used for another compound kind",
            )),
        }
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let value = value.serialize(ValueSerializer)?;
        match &mut self.kind {
            CompoundKind::Map { entries, key } => {
                let key = key.take().ok_or_else(|| {
                    DiagnosticError::new("serializer received a map value without a key")
                })?;
                entries.push((key, value));
                Ok(())
            }
            _ => Err(DiagnosticError::new(
                "map serializer used for another compound kind",
            )),
        }
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::Map { entries, key: None } => Ok(Value::Map(entries)),
            CompoundKind::Map { key: Some(_), .. } => Err(DiagnosticError::new(
                "map serializer ended with a key but no value",
            )),
            _ => Err(DiagnosticError::new(
                "map serializer ended with another kind",
            )),
        }
    }
}

impl SerializeStruct for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.push_field(name, value.serialize(ValueSerializer)?)
    }

    fn skip_field(&mut self, _name: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::Struct { name, fields } => Ok(Value::Struct { name, fields }),
            _ => Err(DiagnosticError::new(
                "struct serializer ended with another kind",
            )),
        }
    }
}

impl SerializeStructVariant for Compound {
    type Ok = Value;
    type Error = DiagnosticError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.push_field(name, value.serialize(ValueSerializer)?)
    }

    fn skip_field(&mut self, _name: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.kind {
            CompoundKind::StructVariant {
                enum_name,
                variant,
                fields,
            } => Ok(Value::StructVariant {
                enum_name,
                variant,
                fields,
            }),
            _ => Err(DiagnosticError::new(
                "struct-variant serializer ended with another kind",
            )),
        }
    }
}
