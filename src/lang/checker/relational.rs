fn aggregate_row(input: &Row, fields: &[AggregateField]) -> Result<Row, TypeError> {
    let mut output = Vec::with_capacity(fields.len());
    for field in fields {
        let source_type = match field.field.as_deref() {
            Some(name) => match input.field(name) {
                Some(column) => column.ty.clone(),
                None if input.columns.is_empty() => Type::Any,
                None => return Err(TypeError::new(format!("unknown field '{name}' in agg"))),
            },
            None => Type::Any,
        };
        let ty = match &field.operation {
            AggregateOp::Named(_) => return Err(TypeError::new("unresolved aggregate operation")),
            AggregateOp::Count => Type::Int,
            AggregateOp::Group | AggregateOp::Min | AggregateOp::Max => source_type,
            AggregateOp::Sum | AggregateOp::Avg => {
                if !matches!(source_type, Type::Int | Type::Float | Type::Any) {
                    return Err(TypeError::new(format!(
                        "aggregate '{}' expects a numeric field",
                        field.alias
                    )));
                }
                if matches!(&field.operation, AggregateOp::Avg) {
                    Type::Float
                } else {
                    source_type
                }
            }
        };
        output.push(Column {
            name: field.alias.clone(),
            ty,
        });
    }
    Ok(Row::new(output))
}

fn join_result_row(left: &Row, right: &Row, kind: &str) -> Row {
    let mut row = right.merge(left);
    let nullable_left = matches!(kind, "__joinRight" | "__joinFull");
    let nullable_right = matches!(kind, "__joinLeft" | "__joinFull");
    for column in &mut row.columns {
        let from_left = left.field(&column.name).is_some();
        if (from_left && nullable_left) || (!from_left && nullable_right) {
            column.ty = make_nullable(column.ty.clone());
        }
    }
    row
}

fn make_nullable(ty: Type) -> Type {
    if matches!(ty, Type::Maybe(_)) {
        ty
    } else {
        Type::Maybe(Box::new(ty))
    }
}

fn check_join_row_expression(expr: &Expr, left: &Row, right: &Row) -> Result<(), TypeError> {
    let Expr::Lambda {
        param: left_param,
        body,
        ..
    } = expr
    else {
        return Err(TypeError::new("join expects a two-argument predicate"));
    };
    let Expr::Lambda {
        param: right_param,
        body,
        ..
    } = body.as_ref()
    else {
        return Err(TypeError::new("join expects a two-argument predicate"));
    };
    let mut scope = HashMap::new();
    scope.insert(left_param.as_str(), left);
    scope.insert(right_param.as_str(), right);
    row_expression_type(body, &scope).map(|_| ())
}

fn mapper_value(expr: &Expr) -> Result<Mapper, TypeError> {
    match expr {
        Expr::Mapper { mapper, .. } => Ok(mapper.clone()),
        Expr::Lambda { param, body, .. } if matches!(body.as_ref(), Expr::Var(name) if name == param) => {
            Ok(Mapper::Identity)
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
                return mapper_value(&reduced);
            }
            match (head, arguments.as_slice()) {
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if name == "__prefix" =>
                {
                    Ok(Mapper::Prefix(value.clone()))
                }
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if name == "__suffix" =>
                {
                    Ok(Mapper::Suffix(value.clone()))
                }
                _ => Err(TypeError::new("expected a mapper value")),
            }
        }
        Expr::Var(name) if name == "__snake" => Ok(Mapper::Snake),
        Expr::Var(name) if name == "__kebab" => Ok(Mapper::Kebab),
        Expr::Var(name) if name == "__camel" => Ok(Mapper::Camel),
        Expr::Var(name) if name == "__maybe" => Ok(Mapper::Maybe),
        Expr::Var(name) if name == "__list" => Ok(Mapper::List),
        _ => Err(TypeError::new("expected a mapper value")),
    }
}

/// Extract aggregate columns from an `agg` row literal: each value must be
/// an aggregate constructor application (`group .user_id`, `sum .total`,
/// `count`). Returns `None` when the expression is not a row literal.
fn aggregate_fields(expr: &Expr) -> Option<Result<Vec<AggregateField>, TypeError>> {
    let fields = match expr {
        Expr::Lambda { body, .. } => match body.as_ref() {
            Expr::RowLiteral(fields) => fields,
            _ => return None,
        },
        Expr::RowLiteral(fields) => fields,
        _ => return None,
    };
    let mut extracted = Vec::with_capacity(fields.len());
    for (alias, value) in fields {
        let extracted_field = match aggregate_key(value) {
            Ok((operation, field)) => {
                if field.is_none() && !matches!(operation, AggregateOp::Count) {
                    return Some(Err(TypeError::new(format!(
                        "aggregate '{alias}' expects a field reference"
                    ))));
                }
                AggregateField {
                    alias: alias.clone(),
                    operation,
                    field,
                }
            }
            Err(error) => return Some(Err(error)),
        };
        extracted.push(extracted_field);
    }
    Some(Ok(extracted))
}

/// Match an aggregate constructor application. The user spellings are matched
/// directly: row literals are opaque to alias expansion, so the prelude
/// names survive to this point.
fn aggregate_key(value: &Expr) -> Result<(AggregateOp, Option<String>), TypeError> {
    let (head, arguments) = flatten_apply(value);
    let head = match head {
        Expr::Annotated { expr, .. } => expr.as_ref(),
        other => other,
    };
    match head {
        Expr::Var(name) => {
            let operation = match name.as_str() {
                "group" | "__group" => AggregateOp::Group,
                "count" | "__count" => AggregateOp::Count,
                "sum" | "__sum" => AggregateOp::Sum,
                "avg" | "__avg" => AggregateOp::Avg,
                "min" | "__min" => AggregateOp::Min,
                "max" | "__max" => AggregateOp::Max,
                _ => {
                    return Err(TypeError::new(format!(
                        "unknown aggregate '{name}'"
                    )))
                }
            };
            let field = arguments.first().and_then(|argument| field_name_of(argument));
            Ok((operation, field))
        }
        // An aliased constructor expands to its wrapper lambda: reduce the
        // application, or step into the body for a bare reference such as
        // `countRows = count`.
        Expr::Lambda { param, body, .. } => {
            if let Some((first, rest)) = arguments.split_first() {
                let mut reduced = substitute(body, param, first);
                for argument in rest {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return aggregate_key(&reduced);
            }
            aggregate_key(body)
        }
        other => Err(TypeError::new(format!(
            "agg expects an aggregate constructor, got {other:?}"
        ))),
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

/// Validate a where predicate: a single-row lambda over the relation row.
/// Named predicates (a plain variable) are only validated by the SQL
/// lowerer, which sees the definition.
fn check_row_expression(expr: &Expr, row: &Row) -> Result<(), TypeError> {
    match expr {
        Expr::Lambda { param, body, .. } => {
            let mut scope = HashMap::new();
            scope.insert(param.as_str(), row);
            row_expression_type(body, &scope).map(|_| ())
        }
        Expr::Annotated { expr, .. } => check_row_expression(expr, row),
        _ => Ok(()),
    }
}

/// Type a scalar expression that SQL lowering will embed, against the row
/// variables it references. Field references must exist in their row and
/// comparison operands must be compatible.
fn row_expression_type(expr: &Expr, scope: &HashMap<&str, &Row>) -> Result<Type, TypeError> {
    match expr {
        Expr::Literal(literal) => Ok(literal_type(literal)),
        Expr::Annotated { expr, .. } => row_expression_type(expr, scope),
        Expr::RowLiteral(fields) => {
            for (_, value) in fields {
                row_expression_type(value, scope)?;
            }
            Ok(Type::Any)
        }
        Expr::List(elements) => {
            for element in elements {
                row_expression_type(element, scope)?;
            }
            Ok(Type::Any)
        }
        // A per-key lambda (from a spaced `.field` inside a list of sort
        // keys) shadows the outer row parameter with its own single row.
        Expr::Lambda { param, body, .. } => {
            let mut scoped = scope.clone();
            if let Some(&shadowed) = scope.get("row") {
                scoped.insert(param.as_str(), shadowed);
            }
            row_expression_type(body, &scoped)
        }
        Expr::Access { target, field } => {
            let Expr::Var(param) = target.as_ref() else {
                return Err(TypeError::new("expected a row field reference"));
            };
            let Some(row) = scope.get(param.as_str()) else {
                return Err(TypeError::new(format!("unknown row variable '{param}'")));
            };
            if row.field(field).is_none() && !row.columns.is_empty() {
                return Err(TypeError::new(format!("unknown field '{field}'")));
            }
            Ok(row.field(field).map_or(Type::Any, |column| column.ty.clone()))
        }
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            // Unwrap annotations, then beta-reduce applied lambdas so wrapper
            // constructors such as `date "..."` reduce to their primitive head.
            let head = match head {
                Expr::Annotated { expr, .. } => expr.as_ref(),
                head => head,
            };
            if let Expr::Lambda { param, body, .. } = head {
                if arguments.is_empty() {
                    return Err(TypeError::new(
                        "expression cannot be used in a SQL predicate",
                    ));
                }
                let mut reduced = substitute(body, param, arguments[0]);
                for argument in &arguments[1..] {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return row_expression_type(&reduced, scope);
            }
            let Expr::Var(name) = head else {
                return Err(TypeError::new(
                    "expression cannot be used in a SQL predicate",
                ));
            };
            let direction_key = matches!(name.as_str(), "__asc" | "__desc");
            if arguments.len() != 2 && !(direction_key && arguments.len() == 1) {
                return Err(TypeError::new(format!(
                    "primitive '{name}' cannot be used in a SQL predicate"
                )));
            }
            match name.as_str() {
                "__asc" | "__desc" => {
                    let value_ty = row_expression_type(arguments[0], scope)?;
                    if !is_orderable_type(&value_ty) {
                        return Err(TypeError::new(format!(
                            "order keys must be orderable values (int, float, string, bool, date, or timestamp), got {value_ty}"
                        )));
                    }
                    Ok(Type::Direction)
                }
                "__eq" | "__ne" | "__lt" | "__le" | "__gt" | "__ge" => {
                    let left_ty = row_expression_type(arguments[0], scope)?;
                    let right_ty = row_expression_type(arguments[1], scope)?;
                    if !compare_operand_compatible(&left_ty, &right_ty) {
                        return Err(TypeError::new(format!(
                            "cannot compare {left_ty} with {right_ty}"
                        )));
                    }
                    Ok(Type::Bool)
                }
                "__and" | "__or" => {
                    for ty in [
                        row_expression_type(arguments[0], scope)?,
                        row_expression_type(arguments[1], scope)?,
                    ] {
                        if !matches!(ty, Type::Bool | Type::Any | Type::Variable(_)) {
                            return Err(TypeError::new(format!(
                                "'{name}' expects boolean operands, got {ty}"
                            )));
                        }
                    }
                    Ok(Type::Bool)
                }
                "__add" | "__sub" | "__mul" | "__div" | "__mod" => {
                    for ty in [
                        row_expression_type(arguments[0], scope)?,
                        row_expression_type(arguments[1], scope)?,
                    ] {
                        if !matches!(ty, Type::Int | Type::Float | Type::Any | Type::Variable(_)) {
                            return Err(TypeError::new(format!(
                                "'{name}' expects numeric operands, got {ty}"
                            )));
                        }
                    }
                    Ok(Type::Any)
                }
                _ => Err(TypeError::new(format!(
                    "primitive '{name}' cannot be used in a SQL predicate"
                ))),
            }
        }
        _ => Err(TypeError::new(
            "expression cannot be used in a SQL predicate",
        )),
    }
}

fn compare_operand_compatible(left: &Type, right: &Type) -> bool {
    type_compatible(left, right)
        || is_nullable_match(left, right)
        || is_nullable_match(right, left)
}

fn is_nullable_match(column: &Type, literal: &Type) -> bool {
    matches!(column, Type::Maybe(inner) if **inner == *literal || *literal == Type::Any)
}

fn literal_type(literal: &Literal) -> Type {
    match literal {
        Literal::String(_) => Type::String,
        Literal::Integer(_) => Type::Int,
        Literal::Float(_) => Type::Float,
        Literal::Bool(_) => Type::Bool,
        Literal::Date(_) => Type::Date,
        Literal::Timestamp(_) => Type::Timestamp,
        Literal::Null => Type::Any,
    }
}

/// Values SQL can sort. Unknown types (Any, variables) pass so open rows and
/// unresolved inference variables are not rejected prematurely.
fn is_orderable_type(ty: &Type) -> bool {
    match ty {
        Type::Int
        | Type::Float
        | Type::String
        | Type::Bool
        | Type::Date
        | Type::Timestamp
        | Type::Any
        | Type::Variable(_) => true,
        Type::Maybe(inner) => is_orderable_type(inner),
        _ => false,
    }
}

