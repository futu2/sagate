use std::{collections::HashMap, fmt};

// ---------- Source model -------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub bindings: Vec<Binding>,
}

/// One imported name: `import { orders as recent_orders } from "./sales.sagate"`
/// introduces `recent_orders` (local) bound to the target's `orders` (source).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportDecl {
    /// Name usable inside this module.
    pub local: String,
    /// Public name looked up in the target module's export table.
    pub source: String,
    /// The written path text, retained for error messages.
    pub path: String,
    /// One-based line of the `import` keyword.
    pub line: usize,
}

/// One entry of an export list or a define-and-export declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportDecl {
    /// Public name other modules import.
    pub public: String,
    /// Local definition or import being published.
    pub local: String,
    /// One-based line of the `export` keyword.
    pub line: usize,
}

/// A top-level binding together with the module metadata the linker needs:
/// where it was declared and whether `export name = ...` published it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedBinding {
    pub line: usize,
    pub exported: bool,
    pub binding: Binding,
}

/// The module-level shape of one parsed source file. Imports and exports are
/// declarations, not bindings; the linker resolves them against other modules.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedModule {
    pub imports: Vec<ImportDecl>,
    pub exports: Vec<ExportDecl>,
    pub bindings: Vec<ParsedBinding>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub name: String,
    pub annotation: Option<Type>,
    pub expr: Expr,
}

/// The core language is deliberately small: values are variables, lambdas,
/// applications, and let bindings. The additional nodes are typed literals
/// understood by the relational prelude (`where`, `select`, and the
/// row mapping functions).
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Var(String),
    Lambda {
        param: String,
        annotation: Option<Type>,
        body: Box<Expr>,
    },
    Apply {
        function: Box<Expr>,
        argument: Box<Expr>,
    },
    Let {
        name: String,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    Annotated {
        expr: Box<Expr>,
        ty: Type,
    },
    /// A SQL expression template. `$1`, `$2`, etc. refer to arguments when
    /// the function is lowered by the SQL backend.
    SqlTemplate(String),
    Literal(Literal),
    Field(String),
    Access {
        target: Box<Expr>,
        field: String,
    },
    /// A row literal: `{x = v1, y = v2}`. Field values are ordinary
    /// expressions; field markers inside them (the `.field` spelling) turn
    /// the literal into a row function, which is how `select` projections
    /// and aggregate projections are written.
    RowLiteral(Vec<(String, Expr)>),
    /// A list literal. Lists appear as the sort-key payload of `order`: the
    /// key function maps a row to a list of `asc`/`desc` tagged values.
    List(Vec<Expr>),
    /// A name with multiple definitions. Overloads are resolved from the
    /// argument type at application time; keeping the cases in the AST also
    /// lets SQL lowering inline the same selected definition.
    Overloaded(Vec<OverloadCase>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverloadCase {
    pub annotation: Option<Type>,
    pub expr: Box<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    String(String),
    Integer(i64),
    Float(String),
    Bool(bool),
    Date(String),
    Timestamp(String),
    Null,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mapper {
    Identity,
    Prefix(String),
    Suffix(String),
    Snake,
    Kebab,
    Camel,
    Maybe,
    List,
}

impl Mapper {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Identity => "id",
            Self::Prefix(_) => "prefix",
            Self::Suffix(_) => "suffix",
            Self::Snake => "snake",
            Self::Kebab => "kebab",
            Self::Camel => "camel",
            Self::Maybe => "maybe",
            Self::List => "list",
        }
    }

    pub fn map_key(&self, key: &str) -> String {
        match self {
            Self::Identity => key.to_owned(),
            Self::Prefix(prefix) => format!("{prefix}{key}"),
            Self::Suffix(suffix) => format!("{key}{suffix}"),
            Self::Snake => snake_case(key, '_'),
            Self::Kebab => snake_case(key, '-'),
            Self::Camel => camel_case(key),
            Self::Maybe | Self::List => key.to_owned(),
        }
    }

    pub fn map_value(&self, ty: Type) -> Type {
        match self {
            Self::Maybe => Type::Maybe(Box::new(ty)),
            Self::List => Type::List(Box::new(ty)),
            _ => ty,
        }
    }
}

fn snake_case(input: &str, separator: char) -> String {
    let mut output = String::with_capacity(input.len());
    for (index, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                output.push(separator);
            }
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

fn camel_case(input: &str) -> String {
    let mut output = String::new();
    let mut upper = false;
    for ch in input.chars() {
        if ch == '_' || ch == '-' {
            upper = true;
        } else if upper {
            output.push(ch.to_ascii_uppercase());
            upper = false;
        } else {
            output.push(ch);
        }
    }
    output
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
#[allow(dead_code)] // Some type terms are produced only by unification paths.
pub enum Type {
    Int,
    String,
    Bool,
    Float,
    Date,
    Timestamp,
    Any,
    Maybe(Box<Type>),
    List(Box<Type>),
    Function(Box<Type>, Box<Type>),
    /// A concrete row value type, distinct from a query over that row.
    Record(Row),
    /// A record value whose row is a type-level expression. This is the
    /// `row` type constructor: it lifts a term of kind `Row` to kind `Type`.
    RowType(Box<RowExpr>),
    /// A row-kind type expression that has not normalized to a concrete row.
    RowExpression(Box<RowExpr>),
    Relation(Row),
    /// A relation whose row is an unapplied type-level expression. Concrete
    /// expressions normalize back to `Relation(Row)` at inference boundaries.
    RelationExpr(Box<RowExpr>),
    /// A relation-level row variable. This is kept distinct from `RowVariable`
    /// because the same source variable can occur either as the row of a
    /// `query` or as the argument/result of an ordinary row function.
    ///
    /// The distinction is internal: source signatures still spell these as
    /// concise variables such as `r` and `s`.
    RelationVariable(u32),
    /// A row itself, used as the argument/result of ordinary predicate and
    /// projection functions.
    RowVariable(u32),
    KeyMapper,
    KeyMapperOf(Box<Type>, Box<Type>),
    KeyMapperWitness(Box<MapperType>),
    ValueMapper,
    ValueMapperOf(Box<Type>, Box<Type>),
    ValueMapperWitness(Box<MapperType>),
    Aggregate(Box<Type>),
    Group(Box<Type>),
    /// The element type of an `order` sort key. `asc`/`desc` tag a key value
    /// with its sort direction; the tagged value erases the key's own type,
    /// which the relational rules validate against the relation's row.
    Direction,
    /// A finite overload set. Unlike a type variable this does not widen to
    /// an arbitrary type: one of the listed function schemes must match.
    Overloaded(Vec<Type>),
    Variable(u32),
}

/// A first-order fragment of the row kind. Keeping these operations as type
/// terms means HM inference can carry an unknown row without erasing the
/// operation that produced it. They normalize once the row is concrete.
#[derive(Clone, Debug, PartialEq)]
pub enum RowExpr {
    Concrete(Row),
    Variable(u32),
    Merge(Box<RowExpr>, Box<RowExpr>),
    MapKey(Box<MapperType>, Box<RowExpr>),
    MapValue(Box<MapperType>, Box<RowExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum MapperType {
    Variable(u32),
    Known(Mapper),
    Unknown,
}

/// Kinds are part of the type language. `Row` is the argument kind of
/// `query`, while key and value mapper witnesses are separate kinds. Keeping
/// the axes distinct prevents a value mapper from ever satisfying `mapKey`
/// (and vice versa), even when both variables have the same printed name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Type,
    Row,
    KeyMapper,
    ValueMapper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum MapperAxis {
    Key,
    Value,
}

impl Mapper {
    pub(super) fn axis(&self) -> MapperAxis {
        match self {
            Self::Identity
            | Self::Prefix(_)
            | Self::Suffix(_)
            | Self::Snake
            | Self::Kebab
            | Self::Camel => MapperAxis::Key,
            Self::Maybe | Self::List => MapperAxis::Value,
        }
    }
}

impl MapperType {
    pub(super) fn kind(&self, axis: MapperAxis) -> Option<Kind> {
        match self {
            Self::Known(Mapper::Identity) => Some(match axis {
                MapperAxis::Key => Kind::KeyMapper,
                MapperAxis::Value => Kind::ValueMapper,
            }),
            Self::Known(mapper) if mapper.axis() != axis => None,
            Self::Known(_) | Self::Variable(_) | Self::Unknown => Some(match axis {
                MapperAxis::Key => Kind::KeyMapper,
                MapperAxis::Value => Kind::ValueMapper,
            }),
        }
    }
}

impl MapperType {
    pub(super) fn normalize(&self) -> Option<Mapper> {
        match self {
            Self::Known(mapper) => Some(mapper.clone()),
            Self::Variable(_) | Self::Unknown => None,
        }
    }
}

impl RowExpr {
    pub(super) fn normalize(&self) -> Option<Row> {
        match self {
            Self::Concrete(row) => Some(row.clone()),
            Self::Variable(_) => None,
            Self::Merge(older, newer) => Some(older.normalize()?.merge(&newer.normalize()?)),
            Self::MapKey(mapper, row) => Some(row.normalize()?.map_key(&mapper.normalize()?)),
            Self::MapValue(mapper, row) => Some(row.normalize()?.map_value(&mapper.normalize()?)),
        }
    }
}

pub(super) fn row_expr_from_type(ty: &Type) -> Option<RowExpr> {
    match ty {
        Type::Relation(row) => Some(RowExpr::Concrete(row.clone())),
        Type::RelationExpr(row) => Some((**row).clone()),
        Type::Any | Type::Variable(_) => None,
        _ => None,
    }
}

pub(super) fn row_expr_from_any_row_type(ty: &Type) -> Option<RowExpr> {
    match ty {
        Type::Record(row) => Some(RowExpr::Concrete(row.clone())),
        Type::RowType(row) => Some((**row).clone()),
        Type::RowVariable(id) => Some(RowExpr::Variable(*id)),
        Type::RowExpression(row) => Some((**row).clone()),
        _ => row_expr_from_type(ty),
    }
}

pub(super) fn mapper_type_from_type(ty: &Type) -> Option<MapperType> {
    match ty {
        Type::KeyMapperWitness(mapper) => Some((**mapper).clone()),
        Type::KeyMapper => Some(MapperType::Unknown),
        _ => None,
    }
}

pub(super) fn value_mapper_type_from_type(ty: &Type) -> Option<MapperType> {
    match ty {
        Type::ValueMapperWitness(mapper) => Some((**mapper).clone()),
        Type::ValueMapper => Some(MapperType::Unknown),
        _ => None,
    }
}

pub(super) fn type_from_row_expr(row: RowExpr) -> Type {
    if let Some(concrete) = row.normalize() {
        Type::Relation(concrete)
    } else {
        Type::RelationExpr(Box::new(row))
    }
}

pub(super) fn type_from_bare_row_expr(row: RowExpr) -> Type {
    if let Some(concrete) = row.normalize() {
        Type::Record(concrete)
    } else {
        Type::RowType(Box::new(row))
    }
}

pub(super) fn relation_of_row_type(ty: Type) -> Option<Type> {
    match ty {
        Type::Record(row) => Some(Type::Relation(row)),
        Type::RowType(row) => Some(Type::RelationExpr(row)),
        Type::RowVariable(id) => Some(Type::RelationExpr(Box::new(RowExpr::Variable(id)))),
        Type::RowExpression(row) => Some(Type::RelationExpr(row)),
        _ => None,
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int => write!(f, "int"),
            Self::String => write!(f, "string"),
            Self::Bool => write!(f, "bool"),
            Self::Float => write!(f, "float"),
            Self::Date => write!(f, "date"),
            Self::Timestamp => write!(f, "timestamp"),
            Self::Any => write!(f, "any"),
            Self::Maybe(inner) => write!(f, "maybe {inner}"),
            Self::List(inner) => write!(f, "list {inner}"),
            Self::Function(argument, result) => write!(f, "({argument} -> {result})"),
            Self::Record(row) => write!(f, "{row}"),
            Self::RowType(row) => write!(f, "row {row}"),
            Self::RowExpression(row) => write!(f, "({row})"),
            Self::Relation(row) => write!(f, "query {row}"),
            Self::RelationExpr(row) => write!(f, "query ({row})"),
            Self::RelationVariable(id) => write!(f, "query {}", type_variable_name(*id)),
            Self::RowVariable(id) => write!(f, "{}", type_variable_name(*id)),
            Self::KeyMapper => write!(f, "keymapper"),
            Self::KeyMapperOf(input, output) => write!(f, "keymapper {input} {output}"),
            Self::KeyMapperWitness(mapper) => write!(f, "keymapper {mapper}"),
            Self::ValueMapper => write!(f, "valuemapper"),
            Self::ValueMapperOf(input, output) => write!(f, "valuemapper {input} {output}"),
            Self::ValueMapperWitness(mapper) => write!(f, "valuemapper {mapper}"),
            Self::Aggregate(inner) => write!(f, "agg {inner}"),
            Self::Group(inner) => write!(f, "group {inner}"),
            Self::Direction => write!(f, "direction"),
            Self::Overloaded(types) => {
                let types = types
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" | ");
                write!(f, "overload ({types})")
            }
            Self::Variable(id) => write!(f, "{}", type_variable_name(*id)),
        }
    }
}

impl fmt::Display for MapperType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variable(id) => write!(f, "κ{}", type_variable_name(*id)),
            Self::Known(mapper) => write!(f, "{}", mapper.type_name()),
            Self::Unknown => write!(f, "_"),
        }
    }
}

impl fmt::Display for RowExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Concrete(row) => write!(f, "{row}"),
            Self::Variable(id) => write!(f, "{}", type_variable_name(*id)),
            Self::Merge(older, newer) => write!(f, "merge {older} {newer}"),
            Self::MapKey(mapper, row) => write!(f, "mapkey {mapper} {row}"),
            Self::MapValue(mapper, row) => write!(f, "mapvalue {mapper} {row}"),
        }
    }
}

pub(super) fn type_variable_name(id: u32) -> String {
    const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
    let letter = ALPHABET[(id as usize) % ALPHABET.len()] as char;
    let generation = id as usize / ALPHABET.len();
    if generation == 0 {
        letter.to_string()
    } else {
        format!("{letter}{generation}")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Row {
    pub columns: Vec<Column>,
    /// `Open` is an existential row tail. It means the visible columns are
    /// known, while additional columns may be present. A row made by a
    /// record annotation is closed; table sources use an open extent.
    pub extent: Extent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Extent {
    #[default]
    Open,
    Closed,
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fields = self
            .columns
            .iter()
            .map(|column| format!("{}: {}", column.name, column.ty))
            .collect::<Vec<_>>()
            .join(", ");
        if self.extent == Extent::Open {
            if fields.is_empty() {
                write!(f, "{{ | r}}")
            } else {
                write!(f, "{{{fields} | r}}")
            }
        } else {
            write!(f, "{{{fields}}}")
        }
    }
}

impl Row {
    pub fn new(columns: Vec<Column>) -> Self {
        let mut result = Vec::with_capacity(columns.len());
        let mut positions = HashMap::new();
        for column in columns {
            if let Some(index) = positions.get(&column.name).copied() {
                result[index] = column;
            } else {
                positions.insert(column.name.clone(), result.len());
                result.push(column);
            }
        }
        Self {
            columns: result,
            extent: Extent::Closed,
        }
    }

    #[allow(dead_code)] // Kept for callers that construct an open catalog row.
    pub fn open(columns: Vec<Column>) -> Self {
        let mut row = Self::new(columns);
        row.extent = Extent::Open;
        row
    }

    pub fn field(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }

    pub fn merge(&self, newer: &Self) -> Self {
        let mut result = self.columns.clone();
        let mut positions: HashMap<String, usize> = result
            .iter()
            .enumerate()
            .map(|(index, column)| (column.name.clone(), index))
            .collect();
        for column in &newer.columns {
            if let Some(index) = positions.get(&column.name).copied() {
                result[index] = column.clone();
            } else {
                positions.insert(column.name.clone(), result.len());
                result.push(column.clone());
            }
        }
        Self {
            columns: result,
            extent: if self.extent == Extent::Open || newer.extent == Extent::Open {
                Extent::Open
            } else {
                Extent::Closed
            },
        }
    }

    pub fn map_key(&self, mapper: &Mapper) -> Self {
        let mut row = Self::new(
            self.columns
                .iter()
                .map(|column| Column {
                    name: mapper.map_key(&column.name),
                    ty: column.ty.clone(),
                })
                .collect(),
        );
        row.extent = self.extent;
        row
    }

    pub fn map_value(&self, mapper: &Mapper) -> Self {
        let mut row = Self::new(
            self.columns
                .iter()
                .map(|column| Column {
                    name: column.name.clone(),
                    ty: mapper.map_value(column.ty.clone()),
                })
                .collect(),
        );
        row.extent = self.extent;
        row
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeError {
    message: String,
    /// The definition being checked when the error surfaced, plus the checker
    /// stage ("inference", "signature", ...). The linker maps the name back
    /// to its original file and spelling for source-facing diagnostics.
    context: Option<(String, String)>,
}

impl TypeError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            context: None,
        }
    }

    pub(super) fn at_definition(mut self, name: &str, stage: &str) -> Self {
        if self.context.is_none() {
            self.context = Some((name.to_owned(), stage.to_owned()));
        }
        self
    }

    pub(super) fn definition(&self) -> Option<&str> {
        self.context.as_ref().map(|(name, _)| name.as_str())
    }
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.context {
            Some((name, stage)) => write!(f, "definition '{name}' {stage}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TypeError {}

#[cfg(test)]
mod mapper_tests {
    use super::*;

    #[test]
    fn snake_kebab_and_camel_apply_ascii_rules_only() {
        // ASCII case and separators transform as before.
        assert_eq!(Mapper::Snake.map_key("firstName"), "first_name");
        assert_eq!(Mapper::Kebab.map_key("firstName"), "first-name");
        assert_eq!(Mapper::Camel.map_key("first_name"), "firstName");
        // Non-ASCII characters pass through unchanged, even around ASCII
        // separators, and no Unicode normalization is applied.
        assert_eq!(Mapper::Snake.map_key("caféId"), "café_id");
        assert_eq!(Mapper::Kebab.map_key("caféId"), "café-id");
        assert_eq!(Mapper::Camel.map_key("café_name"), "caféName");
        assert_eq!(Mapper::Camel.map_key("ελληνικά_όνομα"), "ελληνικάόνομα");
        assert_eq!(Mapper::Snake.map_key("名前"), "名前");
        assert_eq!(Mapper::Snake.map_key("ﬁle"), "ﬁle");
    }

    #[test]
    fn identity_prefix_and_suffix_keep_unicode_text_verbatim() {
        assert_eq!(Mapper::Identity.map_key("名前"), "名前");
        assert_eq!(
            Mapper::Prefix("tbl_".to_owned()).map_key("名前"),
            "tbl_名前"
        );
        assert_eq!(
            Mapper::Suffix("_tmp".to_owned()).map_key("名前"),
            "名前_tmp"
        );
    }
}
