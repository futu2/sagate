//! Relational metadata shared by inference and backends.
//!
//! The core AST only knows a generic foreign-call surface: every backend
//! operation is a declaration carrying an opaque [`ForeignId`]. The type
//! checker and the SQL lowerer dispatch on that id instead of matching
//! name strings, so public spellings stay ordinary Sagate names. Keeping
//! these values outside `ast.rs` leaves the core syntax tree focused on
//! language expressions and row values.

use std::collections::HashMap;

use super::ast::{Binding, Expr, Mapper, MapperAxis, Row, Type};
use super::ast::Literal;
use super::checker::{flatten_apply, substitute};

/// The backend operation a declaration lowers to. The id is the only handle
/// the compiler pipeline keeps: names can be rebound, aliased, or overridden
/// without changing how an application is recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ForeignId {
    Table,
    Where,
    Select,
    MapKey,
    MapValue,
    Merge,
    Aggregate,
    JoinInner,
    JoinLeft,
    JoinRight,
    JoinFull,
    Order,
    Limit,
    Asc,
    Desc,
    Snake,
    Kebab,
    Camel,
    Prefix,
    Suffix,
    Maybe,
    List,
    Group,
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl ForeignId {
    /// The operation declared under a written name. Declaration spellings and
    /// the public aggregate constructors share one table: `foreign sql where`
    /// and `__where` both declare `Where`, and the stdlib aggregate names are
    /// the same operations under their public names.
    pub(crate) fn from_decl_name(name: &str) -> Option<Self> {
        Some(match name {
            "table" | "__table" => Self::Table,
            "where" | "__where" => Self::Where,
            "select" | "__select" => Self::Select,
            "mapKey" | "__mapKey" => Self::MapKey,
            "mapValue" | "__mapValue" => Self::MapValue,
            "merge" | "__merge" => Self::Merge,
            "agg" | "__agg" => Self::Aggregate,
            "joinInner" | "__joinInner" => Self::JoinInner,
            "joinLeft" | "__joinLeft" => Self::JoinLeft,
            "joinRight" | "__joinRight" => Self::JoinRight,
            "joinFull" | "__joinFull" => Self::JoinFull,
            "order" | "__order" => Self::Order,
            "limit" | "__limit" => Self::Limit,
            "asc" | "__asc" => Self::Asc,
            "desc" | "__desc" => Self::Desc,
            "snake" | "__snake" => Self::Snake,
            "kebab" | "__kebab" => Self::Kebab,
            "camel" | "__camel" => Self::Camel,
            "prefix" | "__prefix" => Self::Prefix,
            "suffix" | "__suffix" => Self::Suffix,
            "maybe" | "__maybe" => Self::Maybe,
            "list" | "__list" => Self::List,
            "group" | "__group" => Self::Group,
            "count" | "__count" => Self::Count,
            "sum" | "__sum" => Self::Sum,
            "avg" | "__avg" => Self::Avg,
            "min" | "__min" => Self::Min,
            "max" | "__max" => Self::Max,
            _ => return None,
        })
    }

    pub(crate) fn is_join(self) -> bool {
        matches!(
            self,
            Self::JoinInner | Self::JoinLeft | Self::JoinRight | Self::JoinFull
        )
    }
}

/// Foreign declarations of one program, keyed by binding symbol. The checker
/// and the SQL backend build this once per compilation and dispatch on it.
pub(crate) type ForeignOps = HashMap<String, ForeignId>;

pub(crate) fn foreign_declarations(bindings: &[Binding]) -> ForeignOps {
    bindings
        .iter()
        .filter_map(|binding| binding.foreign.map(|id| (binding.name.clone(), id)))
        .collect()
}

/// Definition bodies available to aggregate and mapper extraction. The SQL
/// backend passes the program's bindings so aliased constructors can be
/// chased; the checker inspects already-expanded expressions and passes an
/// empty table.
pub(crate) type Definitions<'a> = HashMap<String, &'a Expr>;

pub(crate) fn no_definitions() -> Definitions<'static> {
    HashMap::new()
}

/// Combine rows for a join, marking columns from an outer-joined side as
/// nullable. This backend rule stays alongside the join metadata instead of
/// becoming part of the syntax tree's row model.
pub(crate) fn join_row(left: &Row, right: &Row, kind: ForeignId) -> Row {
    let mut row = right.merge(left);
    let nullable_left = matches!(kind, ForeignId::JoinRight | ForeignId::JoinFull);
    let nullable_right = matches!(kind, ForeignId::JoinLeft | ForeignId::JoinFull);
    for column in &mut row.columns {
        let from_left = left.field(&column.name).is_some();
        if ((from_left && nullable_left) || (!from_left && nullable_right))
            && !matches!(column.ty, Type::Maybe(_))
        {
            column.ty = Type::Maybe(Box::new(column.ty.clone()));
        }
    }
    row
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AggregateOp {
    Group,
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// One extracted aggregate column: `{total = sum .total}` contributes
/// `(total, Sum, Some("total"))`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AggregateField {
    pub(crate) alias: String,
    pub(crate) operation: AggregateOp,
    pub(crate) function: Option<String>,
    pub(crate) field: Option<String>,
}

/// The row literal behind a select projection or aggregate projection:
/// either the literal itself or a single-row lambda whose body is one. The
/// lambda parameter names the row its field expressions read from.
pub(crate) fn row_literal_fields(expr: &Expr) -> Option<(&str, &Vec<(String, Expr)>)> {
    match expr {
        Expr::RowLiteral(fields) => Some(("row", fields)),
        Expr::Annotated { expr, .. } => row_literal_fields(expr),
        Expr::Lambda { param, body, .. } => match body.as_ref() {
            Expr::RowLiteral(fields) => Some((param.as_str(), fields)),
            _ => None,
        },
        _ => None,
    }
}

/// Extract aggregate columns from an `agg` row literal: each value must be
/// an aggregate constructor application (`group .user_id`, `sum .total`,
/// `count`). Returns `None` when the expression is not a row literal.
pub(crate) fn aggregate_row_fields(
    expr: &Expr,
    definitions: &Definitions,
    foreign: &ForeignOps,
) -> Option<Result<Vec<AggregateField>, String>> {
    let (_, fields) = row_literal_fields(expr)?;
    let mut extracted = Vec::with_capacity(fields.len());
    for (alias, value) in fields {
        let field = match aggregate_key(value, definitions, foreign) {
            Ok((operation, function, field)) => {
                if field.is_none() && !matches!(operation, AggregateOp::Count) {
                    return Some(Err(format!(
                        "aggregate '{alias}' expects a field reference"
                    )));
                }
                AggregateField {
                    alias: alias.clone(),
                    operation,
                    function,
                    field,
                }
            }
            Err(error) => return Some(Err(error)),
        };
        extracted.push(field);
    }
    Some(Ok(extracted))
}

/// Match an aggregate constructor application. Row literals carry user
/// spellings, so bindings are chased to the foreign constructor they bottom
/// out at; a declared foreign name (including the public aggregate names)
/// terminates the chase.
fn aggregate_key(
    value: &Expr,
    definitions: &Definitions,
    foreign: &ForeignOps,
) -> Result<(AggregateOp, Option<String>, Option<String>), String> {
    let (head, arguments) = flatten_apply(value);
    let head = match head {
        Expr::Annotated { expr, .. } => expr.as_ref(),
        other => other,
    };
    match head {
        Expr::Var(name) => {
            // Bindings take precedence over the fixed names, so user
            // overrides of the prelude constructors apply here too. A
            // declared foreign operation is the chase's terminal.
            if !foreign.contains_key(name) {
                if let Some(definition) = definitions.get(name) {
                    let mut expanded = (*definition).clone();
                    for argument in &arguments {
                        expanded = Expr::Apply {
                            function: Box::new(expanded),
                            argument: Box::new((*argument).clone()),
                        };
                    }
                    return aggregate_key(&expanded, definitions, foreign);
                }
            }
            let id = foreign
                .get(name)
                .copied()
                .or_else(|| declared_operation(name));
            let operation = match id {
                Some(ForeignId::Group) => AggregateOp::Group,
                Some(ForeignId::Count) => AggregateOp::Count,
                Some(ForeignId::Sum) => AggregateOp::Sum,
                Some(ForeignId::Avg) => AggregateOp::Avg,
                Some(ForeignId::Min) => AggregateOp::Min,
                Some(ForeignId::Max) => AggregateOp::Max,
                _ => return Err(format!("unknown aggregate '{name}'")),
            };
            let field = arguments.first().and_then(|argument| field_name_of(argument));
            // The function keeps the terminal binding symbol so the SQL
            // backend can find that binding's template body.
            let function = (!matches!(operation, AggregateOp::Group)).then(|| name.clone());
            Ok((operation, function, field))
        }
        // An aliased constructor reduces through its wrapper lambda: applied
        // arguments beta-reduce, a bare reference (`countRows = count`)
        // continues with the wrapper body.
        Expr::Lambda { param, body, .. } => {
            if let Some((first, rest)) = arguments.split_first() {
                let mut reduced = substitute(body, param, first);
                for argument in rest {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return aggregate_key(&reduced, definitions, foreign);
            }
            aggregate_key(body, definitions, foreign)
        }
        other => Err(format!(
            "agg expects an aggregate constructor, got {other:?}"
        )),
    }
}

/// The foreign operation a written symbol declares, if any. Module-qualified
/// symbols (`$prelude:sum`) reduce to their written name; double-underscore
/// primitives keep their spelling.
fn declared_operation(symbol: &str) -> Option<ForeignId> {
    let written = super::parser::prelude_source_name(symbol);
    ForeignId::from_decl_name(written.strip_prefix("__").unwrap_or(written))
}

/// Resolve the concrete mapper behind a `mapKey`/`mapValue` argument. The
/// expression is matched structurally: identity lambdas, parameterized
/// prefix/suffix applications, and declared mapper operations. Aliased
/// mappers chase through their definitions.
pub(crate) fn mapper_of(
    expr: &Expr,
    definitions: &Definitions,
    foreign: &ForeignOps,
    axis: MapperAxis,
) -> Result<Mapper, String> {
    let key = matches!(axis, MapperAxis::Key);
    let axis_error = || {
        if key {
            "mapKey expects a key mapper".to_owned()
        } else {
            "mapValue expects a value mapper".to_owned()
        }
    };
    match expr {
        Expr::Lambda { param, body, .. }
            if matches!(body.as_ref(), Expr::Var(name) if name == param) =>
        {
            Ok(Mapper::Identity)
        }
        // Chase bindings to their primitive head; declared foreign
        // operations are the primitives themselves and must not be chased.
        Expr::Var(name) if !foreign.contains_key(name) && definitions.contains_key(name) => {
            mapper_of(definitions[name], definitions, foreign, axis)
        }
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            if let Expr::Lambda { param, body, .. } = head {
                let mut reduced = substitute(body, param, arguments[0]);
                for argument in &arguments[1..] {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return mapper_of(&reduced, definitions, foreign, axis);
            }
            if let Expr::Var(name) = head {
                if !foreign.contains_key(name) {
                    if let Some(definition) = definitions.get(name) {
                        let mut expanded = (*definition).clone();
                        for argument in arguments {
                            expanded = Expr::Apply {
                                function: Box::new(expanded),
                                argument: Box::new((*argument).clone()),
                            };
                        }
                        return mapper_of(&expanded, definitions, foreign, axis);
                    }
                }
            }
            match (head, arguments.as_slice()) {
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if key && foreign.get(name) == Some(&ForeignId::Prefix) =>
                {
                    Ok(Mapper::Prefix(value.clone()))
                }
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if key && foreign.get(name) == Some(&ForeignId::Suffix) =>
                {
                    Ok(Mapper::Suffix(value.clone()))
                }
                _ => Err(axis_error()),
            }
        }
        Expr::Var(name) => match foreign.get(name).copied() {
            Some(ForeignId::Snake) => Ok(Mapper::Snake),
            Some(ForeignId::Kebab) => Ok(Mapper::Kebab),
            Some(ForeignId::Camel) => Ok(Mapper::Camel),
            Some(ForeignId::Maybe) => Ok(Mapper::Maybe),
            Some(ForeignId::List) => Ok(Mapper::List),
            _ => Err(axis_error()),
        },
        _ => Err(axis_error()),
    }
}

fn field_name_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Field(field) => Some(field.clone()),
        Expr::Access { field, .. } => Some(field.clone()),
        Expr::Lambda { body, .. } => field_name_of(body),
        _ => None,
    }
}
