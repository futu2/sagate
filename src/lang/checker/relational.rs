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

fn check_join_row_expression(
    expr: &Expr,
    left: &Row,
    right: &Row,
    foreign: &ForeignOps,
) -> Result<(), TypeError> {
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
    row_expression_type(body, &scope, foreign).map(|_| ())
}

/// Validate a where predicate: a single-row lambda over the relation row.
/// Named predicates (a plain variable) are only validated by the SQL
/// lowerer, which sees the definition.
fn check_row_expression(expr: &Expr, row: &Row, foreign: &ForeignOps) -> Result<(), TypeError> {
    match expr {
        Expr::Lambda { param, body, .. } => {
            let mut scope = HashMap::new();
            scope.insert(param.as_str(), row);
            row_expression_type(body, &scope, foreign).map(|_| ())
        }
        Expr::Annotated { expr, .. } => check_row_expression(expr, row, foreign),
        _ => Ok(()),
    }
}

/// Scalar operator spellings that can appear inside SQL predicates. The
/// operators are ordinary overloaded template functions; this classification
/// only drives operand validation until it moves into the backend pass.
enum PredicateOperator {
    Compare,
    Logical,
    Arithmetic,
}

fn predicate_operator(spelling: &str) -> Option<PredicateOperator> {
    match spelling {
        "==" | "!=" | "<" | "<=" | ">" | ">=" => Some(PredicateOperator::Compare),
        "&&" | "||" => Some(PredicateOperator::Logical),
        "+" | "-" | "*" | "/" | "%" => Some(PredicateOperator::Arithmetic),
        _ => None,
    }
}

/// Type a scalar expression that SQL lowering will embed, against the row
/// variables it references. Field references must exist in their row and
/// comparison operands must be compatible. Foreign operations are recognized
/// by their declared id; scalar operators by their written spelling.
fn row_expression_type(
    expr: &Expr,
    scope: &HashMap<&str, &Row>,
    foreign: &ForeignOps,
) -> Result<Type, TypeError> {
    match expr {
        Expr::Literal(literal) => Ok(literal_type(literal)),
        Expr::Annotated { expr, .. } => row_expression_type(expr, scope, foreign),
        Expr::RowLiteral(fields) => {
            for (_, value) in fields {
                row_expression_type(value, scope, foreign)?;
            }
            Ok(Type::Any)
        }
        Expr::List(elements) => {
            for element in elements {
                row_expression_type(element, scope, foreign)?;
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
            row_expression_type(body, &scoped, foreign)
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
            // constructors such as `date "..."` reduce to their foreign head.
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
                return row_expression_type(&reduced, scope, foreign);
            }
            let Expr::Var(name) = head else {
                return Err(TypeError::new(
                    "expression cannot be used in a SQL predicate",
                ));
            };
            let op = foreign.get(name).copied();
            let written = prelude_source_name(name);
            let scalar = if op.is_none() {
                predicate_operator(written)
            } else {
                None
            };
            // Linked references to user bindings carry internal symbols;
            // anything that is neither a foreign operation nor a scalar
            // operator lowers as an opaque value.
            if op.is_none() && scalar.is_none() && !written.starts_with("__") {
                for argument in arguments {
                    row_expression_type(argument, scope, foreign)?;
                }
                return Ok(Type::Any);
            }
            let direction_key = matches!(op, Some(ForeignId::Asc | ForeignId::Desc));
            if arguments.len() != 2 && !(direction_key && arguments.len() == 1) {
                return Err(TypeError::new(format!(
                    "primitive '{written}' cannot be used in a SQL predicate"
                )));
            }
            match (op, scalar) {
                (Some(ForeignId::Asc | ForeignId::Desc), _) => {
                    let value_ty = row_expression_type(arguments[0], scope, foreign)?;
                    if !is_orderable_type(&value_ty) {
                        return Err(TypeError::new(format!(
                            "order keys must be orderable values (int, float, string, bool, date, or timestamp), got {value_ty}"
                        )));
                    }
                    Ok(Type::Direction)
                }
                (_, Some(PredicateOperator::Compare)) => {
                    let left_ty = row_expression_type(arguments[0], scope, foreign)?;
                    let right_ty = row_expression_type(arguments[1], scope, foreign)?;
                    if !compare_operand_compatible(&left_ty, &right_ty) {
                        return Err(TypeError::new(format!(
                            "cannot compare {left_ty} with {right_ty}"
                        )));
                    }
                    Ok(Type::Bool)
                }
                (_, Some(PredicateOperator::Logical)) => {
                    for ty in [
                        row_expression_type(arguments[0], scope, foreign)?,
                        row_expression_type(arguments[1], scope, foreign)?,
                    ] {
                        if !matches!(ty, Type::Bool | Type::Any | Type::Variable(_)) {
                            return Err(TypeError::new(format!(
                                "'{written}' expects boolean operands, got {ty}"
                            )));
                        }
                    }
                    Ok(Type::Bool)
                }
                (_, Some(PredicateOperator::Arithmetic)) => {
                    for ty in [
                        row_expression_type(arguments[0], scope, foreign)?,
                        row_expression_type(arguments[1], scope, foreign)?,
                    ] {
                        if !matches!(ty, Type::Int | Type::Float | Type::Any | Type::Variable(_)) {
                            return Err(TypeError::new(format!(
                                "'{written}' expects numeric operands, got {ty}"
                            )));
                        }
                    }
                    Ok(Type::Any)
                }
                _ => Err(TypeError::new(format!(
                    "primitive '{written}' cannot be used in a SQL predicate"
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
