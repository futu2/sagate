// Definition tables and the `Expr`/`HashMap`/`ForeignOps`/`Mapper` types are
// in scope through the shared compiler module (see entry.rs).

/// The `(row parameter, fields)` shape of a select projection.
type ProjectionFields<'a> = (&'a str, &'a Vec<(String, Expr)>);

/// The row literal behind a select projection: either the literal itself or
/// a single-row lambda whose body is one. The parameter names the row the
/// field expressions read from; SQL lowering maps it to the subquery alias.
fn projection_fields(expr: &Expr) -> Option<ProjectionFields<'_>> {
    row_literal_fields(expr)
}

/// Extract aggregate columns from an `agg` row literal: each value must be
/// an aggregate constructor application (`group .user_id`, `sum .total`,
/// `count`).
fn aggregate_projection(
    expr: &Expr,
    definitions: &HashMap<String, &Expr>,
    foreign: &ForeignOps,
) -> Result<Vec<AggregateField>, String> {
    aggregate_row_fields(expr, definitions, foreign)
        .ok_or_else(|| "agg expects an aggregate projection".to_owned())?
}

/// Resolve the concrete mapper behind a `mapKey`/`mapValue` argument.
fn mapper_value(
    expr: &Expr,
    key: bool,
    definitions: &HashMap<String, &Expr>,
    foreign: &ForeignOps,
) -> Result<Mapper, String> {
    let axis = if key {
        MapperAxis::Key
    } else {
        MapperAxis::Value
    };
    mapper_of(expr, definitions, foreign, axis)
}

pub(super) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
