//! Relational metadata shared by inference and backends.
//!
//! These values describe relational operations after the ordinary expression
//! parser has run. Keeping them outside `ast.rs` leaves the core syntax tree
//! focused on language expressions and row values.

use super::ast::{Row, Type};

/// Names understood by the relational backend. Keeping this mapping in one
/// place avoids making the parser, checker, and SQL lowerer each maintain a
/// separate list of string literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Intrinsic {
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
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl Intrinsic {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "__table" => Self::Table,
            "__where" => Self::Where,
            "__select" => Self::Select,
            "__mapKey" => Self::MapKey,
            "__mapValue" => Self::MapValue,
            "__merge" => Self::Merge,
            "__agg" => Self::Aggregate,
            "__joinInner" => Self::JoinInner,
            "__joinLeft" => Self::JoinLeft,
            "__joinRight" => Self::JoinRight,
            "__joinFull" => Self::JoinFull,
            "__order" => Self::Order,
            "__limit" => Self::Limit,
            "__asc" => Self::Asc,
            "__desc" => Self::Desc,
            "__snake" => Self::Snake,
            "__kebab" => Self::Kebab,
            "__camel" => Self::Camel,
            "__prefix" => Self::Prefix,
            "__suffix" => Self::Suffix,
            "__maybe" => Self::Maybe,
            "__list" => Self::List,
            "__group" => Self::Group,
            "__count" => Self::Count,
            "__sum" => Self::Sum,
            "__avg" => Self::Avg,
            "__min" => Self::Min,
            "__max" => Self::Max,
            "__add" => Self::Add,
            "__sub" => Self::Sub,
            "__mul" => Self::Mul,
            "__div" => Self::Div,
            "__mod" => Self::Mod,
            "__eq" => Self::Eq,
            "__ne" => Self::Ne,
            "__lt" => Self::Lt,
            "__le" => Self::Le,
            "__gt" => Self::Gt,
            "__ge" => Self::Ge,
            "__and" => Self::And,
            "__or" => Self::Or,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Table => "__table",
            Self::Where => "__where",
            Self::Select => "__select",
            Self::MapKey => "__mapKey",
            Self::MapValue => "__mapValue",
            Self::Merge => "__merge",
            Self::Aggregate => "__agg",
            Self::JoinInner => "__joinInner",
            Self::JoinLeft => "__joinLeft",
            Self::JoinRight => "__joinRight",
            Self::JoinFull => "__joinFull",
            Self::Order => "__order",
            Self::Limit => "__limit",
            Self::Asc => "__asc",
            Self::Desc => "__desc",
            Self::Snake => "__snake",
            Self::Kebab => "__kebab",
            Self::Camel => "__camel",
            Self::Prefix => "__prefix",
            Self::Suffix => "__suffix",
            Self::Maybe => "__maybe",
            Self::List => "__list",
            Self::Group => "__group",
            Self::Count => "__count",
            Self::Sum => "__sum",
            Self::Avg => "__avg",
            Self::Min => "__min",
            Self::Max => "__max",
            Self::Add => "__add",
            Self::Sub => "__sub",
            Self::Mul => "__mul",
            Self::Div => "__div",
            Self::Mod => "__mod",
            Self::Eq => "__eq",
            Self::Ne => "__ne",
            Self::Lt => "__lt",
            Self::Le => "__le",
            Self::Gt => "__gt",
            Self::Ge => "__ge",
            Self::And => "__and",
            Self::Or => "__or",
        }
    }

    pub(crate) fn from_public_name(name: &str) -> Option<Self> {
        Self::from_name(name).or_else(|| {
            Some(match name {
                "group" => Self::Group,
                "count" => Self::Count,
                "sum" => Self::Sum,
                "avg" => Self::Avg,
                "min" => Self::Min,
                "max" => Self::Max,
                _ => return None,
            })
        })
    }

    pub(crate) fn from_operator(name: &str) -> Option<Self> {
        Some(match name {
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" => Self::Mul,
            "/" => Self::Div,
            "%" => Self::Mod,
            "==" => Self::Eq,
            "!=" => Self::Ne,
            "<" => Self::Lt,
            "<=" => Self::Le,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "&&" => Self::And,
            "||" => Self::Or,
            _ => return None,
        })
    }

    pub(crate) fn is_scalar(self) -> bool {
        matches!(
            self,
            Self::Eq
                | Self::Ne
                | Self::Lt
                | Self::Le
                | Self::Gt
                | Self::Ge
                | Self::Add
                | Self::Sub
                | Self::Mul
                | Self::Div
                | Self::Mod
                | Self::And
                | Self::Or
        )
    }

    pub(crate) fn is_join(self) -> bool {
        matches!(
            self,
            Self::JoinInner | Self::JoinLeft | Self::JoinRight | Self::JoinFull
        )
    }
}

/// Combine rows for a join, marking columns from an outer-joined side as
/// nullable. This backend rule stays alongside the join intrinsic metadata
/// instead of becoming part of the syntax tree's row model.
pub(crate) fn join_row(left: &Row, right: &Row, kind: Intrinsic) -> Row {
    let mut row = right.merge(left);
    let nullable_left = matches!(kind, Intrinsic::JoinRight | Intrinsic::JoinFull);
    let nullable_right = matches!(kind, Intrinsic::JoinLeft | Intrinsic::JoinFull);
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
