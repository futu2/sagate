
/// Maps the row parameters of a predicate to the SQL alias and schema of the
/// relation they denote ("q" for where, "l"/"r" for joins).
struct RowScope<'a> {
    params: Vec<(String, &'a str, &'a Row)>,
}

impl<'a> RowScope<'a> {
    fn get(&self, param: &str) -> Option<(&'a str, &'a Row)> {
        self.params
            .iter()
            .find(|(name, _, _)| name == param)
            .map(|(_, alias, row)| (*alias, *row))
    }
}

/// Lower a where predicate: a single-row lambda over the "q" subquery alias.
pub(super) fn where_condition(
    expr: &Expr,
    row: &Row,
    definitions: &HashMap<String, &Expr>,
) -> Result<SqlExpr, String> {
    match expr {
        Expr::Lambda { param, body, .. } => {
            let scope = RowScope {
                params: vec![(param.clone(), "q", row)],
            };
            lower_row_expression(body, &scope, definitions)
        }
        Expr::Annotated { expr, .. } => where_condition(expr, row, definitions),
        Expr::Var(name) => {
            let definition = definitions
                .get(name)
                .ok_or_else(|| format!("unknown predicate '{name}'"))?;
            where_condition(definition, row, definitions)
        }
        _ => Err("where expects a predicate".to_owned()),
    }
}

/// Lower a join predicate: a two-row lambda over the "l"/"r" subquery aliases.
pub(super) fn join_condition(
    expr: &Expr,
    left: &Row,
    right: &Row,
    definitions: &HashMap<String, &Expr>,
) -> Result<SqlExpr, String> {
    let Expr::Lambda {
        param: left_param,
        body,
        ..
    } = expr
    else {
        return Err("join expects a two-argument predicate".to_owned());
    };
    let Expr::Lambda {
        param: right_param,
        body,
        ..
    } = body.as_ref()
    else {
        return Err("join expects a two-argument predicate".to_owned());
    };
    let scope = RowScope {
        params: vec![
            (left_param.clone(), "l", left),
            (right_param.clone(), "r", right),
        ],
    };
    lower_row_expression(body, &scope, definitions)
}

/// Lower order keys: a row lambda whose body is a list of `asc`/`desc`
/// tagged key values, into ORDER BY items over the "q" subquery alias.
pub(super) fn order_by_items(
    expr: &Expr,
    row: &Row,
    definitions: &HashMap<String, &Expr>,
) -> Result<Vec<OrderByItem>, String> {
    match expr {
        Expr::Lambda { param, body, .. } => {
            let scope = RowScope {
                params: vec![(param.clone(), "q", row)],
            };
            let elements = match body.as_ref() {
                Expr::List(elements) => elements,
                // A single tagged key without brackets still denotes a
                // one-element list.
                single @ Expr::Apply { .. } => std::slice::from_ref(single),
                _ => return Err("order keys must be asc or desc values".to_owned()),
            };
            let mut items = Vec::with_capacity(elements.len());
            for element in elements {
                let (ascending, value) = resolve_order_key(element, definitions)?;
                let key = lower_row_expression(&value, &scope, definitions)?;
                items.push(OrderByItem {
                    expr: key,
                    ascending,
                    nulls_first: None,
                });
            }
            Ok(items)
        }
        Expr::Annotated { expr, .. } => order_by_items(expr, row, definitions),
        Expr::Var(name) => {
            let definition = definitions
                .get(name)
                .ok_or_else(|| format!("unknown sort keys '{name}'"))?;
            order_by_items(definition, row, definitions)
        }
        _ => Err("order expects sort keys".to_owned()),
    }
}

/// Reduce one list element to its direction and the key value it tags,
/// chasing named wrappers such as `asc` down to their primitives.
fn resolve_order_key(
    element: &Expr,
    definitions: &HashMap<String, &Expr>,
) -> Result<(bool, Expr), String> {
    let (head, arguments) = flatten_apply(element);
    if let Expr::Var(name) = head {
        match name.as_str() {
            "__asc" if arguments.len() == 1 => {
                return Ok((true, arguments[0].clone()));
            }
            "__desc" if arguments.len() == 1 => {
                return Ok((false, arguments[0].clone()));
            }
            _ if !name.starts_with("__") => {
                let definition = definitions
                    .get(name)
                    .ok_or_else(|| format!("unknown function '{name}'"))?;
                let mut expanded = (*definition).clone();
                for argument in &arguments {
                    expanded = Expr::Apply {
                        function: Box::new(expanded),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return resolve_order_key(&expanded, definitions);
            }
            _ => {}
        }
    }
    if let Expr::Lambda { param, body, .. } = head {
        if let Some((first, rest)) = arguments.split_first() {
            let mut reduced = substitute(body, param, first);
            for argument in rest {
                reduced = Expr::Apply {
                    function: Box::new(reduced),
                    argument: Box::new((*argument).clone()),
                };
            }
            return resolve_order_key(&reduced, definitions);
        }
    }
    Err("order keys must be asc or desc values".to_owned())
}

fn lower_row_expression(
    expr: &Expr,
    scope: &RowScope,
    definitions: &HashMap<String, &Expr>,
) -> Result<SqlExpr, String> {
    match expr {
        Expr::Literal(literal) => Ok(sql_literal(literal)),
        Expr::Annotated { expr, .. } => lower_row_expression(expr, scope, definitions),
        // A per-key lambda (from a spaced `.field` inside a list of sort
        // keys) shadows the outer row parameter; keep its alias binding.
        Expr::Lambda { param, body, .. } => {
            let mut params = Vec::with_capacity(scope.params.len());
            for (name, alias, row) in &scope.params {
                if name == param {
                    params.push((param.clone(), *alias, *row));
                } else {
                    params.push((name.clone(), *alias, *row));
                }
            }
            lower_row_expression(body, &RowScope { params }, definitions)
        }
        Expr::Access { target, field } => {
            let Expr::Var(param) = target.as_ref() else {
                return Err("expected a row field reference".to_owned());
            };
            let Some((alias, row)) = scope.get(param) else {
                return Err(format!("unknown row variable '{param}'"));
            };
            if row.field(field).is_none() && !row.columns.is_empty() {
                return Err(format!("unknown field '{field}'"));
            }
            Ok(sql_column(field, Some(alias)))
        }
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            if let Expr::Lambda { param, body, .. } = head {
                if arguments.is_empty() {
                    return Err("expression cannot be lowered to SQL".to_owned());
                }
                let mut reduced = substitute(body, param, arguments[0]);
                for argument in &arguments[1..] {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return lower_row_expression(&reduced, scope, definitions);
            }
            let Expr::Var(name) = head else {
                return Err("expression cannot be lowered to SQL".to_owned());
            };
            // Chase named bindings to the primitive they bottom out at.
            if !name.starts_with("__") {
                let Some(definition) = definitions.get(name) else {
                    return Err(format!("unknown function '{name}'"));
                };
                let mut expanded = (*definition).clone();
                for argument in arguments {
                    expanded = Expr::Apply {
                        function: Box::new(expanded),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return lower_row_expression(&expanded, scope, definitions);
            }
            match (name.as_str(), arguments.len()) {
                ("__eq" | "__ne" | "__lt" | "__le" | "__gt" | "__ge", 2) => {
                    let operator = comparison_operator(name).unwrap();
                    let left = lower_row_expression(arguments[0], scope, definitions)?;
                    let right = lower_row_expression(arguments[1], scope, definitions)?;
                    comparison_sql(operator, left, right)
                }
                ("__and" | "__or", 2) => {
                    let operator = if name == "__and" {
                        BinaryOperator::And
                    } else {
                        BinaryOperator::Or
                    };
                    let left = lower_row_expression(arguments[0], scope, definitions)?;
                    let right = lower_row_expression(arguments[1], scope, definitions)?;
                    Ok(SqlExpr::BinaryOp {
                        left: Box::new(left),
                        op: operator,
                        right: Box::new(right),
                    })
                }
                ("__add" | "__sub" | "__mul" | "__div" | "__mod", 2) => {
                    let operator = match name.as_str() {
                        "__add" => BinaryOperator::Plus,
                        "__sub" => BinaryOperator::Minus,
                        "__mul" => BinaryOperator::Multiply,
                        "__div" => BinaryOperator::Divide,
                        _ => BinaryOperator::Modulo,
                    };
                    let left = lower_row_expression(arguments[0], scope, definitions)?;
                    let right = lower_row_expression(arguments[1], scope, definitions)?;
                    Ok(SqlExpr::BinaryOp {
                        left: Box::new(left),
                        op: operator,
                        right: Box::new(right),
                    })
                }
                _ => Err(format!("primitive '{name}' cannot be lowered to SQL")),
            }
        }
        _ => Err("expression cannot be lowered to SQL".to_owned()),
    }
}

/// Comparisons against null become IS [NOT] NULL.
fn comparison_sql(
    operator: BinaryOperator,
    left: SqlExpr,
    right: SqlExpr,
) -> Result<SqlExpr, String> {
    let left_is_null = matches!(left, SqlExpr::Null);
    let right_is_null = matches!(right, SqlExpr::Null);
    if left_is_null || right_is_null {
        let negated = match operator {
            BinaryOperator::Eq => false,
            BinaryOperator::Neq => true,
            _ => return Err("null can only be compared with == or !=".to_owned()),
        };
        let operand = if left_is_null { right } else { left };
        return Ok(SqlExpr::IsNull {
            expr: Box::new(operand),
            negated,
        });
    }
    Ok(SqlExpr::BinaryOp {
        left: Box::new(left),
        op: operator,
        right: Box::new(right),
    })
}

fn comparison_operator(name: &str) -> Option<BinaryOperator> {
    match name {
        "__eq" => Some(BinaryOperator::Eq),
        "__ne" => Some(BinaryOperator::Neq),
        "__lt" => Some(BinaryOperator::Lt),
        "__le" => Some(BinaryOperator::LtEq),
        "__gt" => Some(BinaryOperator::Gt),
        "__ge" => Some(BinaryOperator::GtEq),
        _ => None,
    }
}


fn sql_literal(literal: &Literal) -> SqlExpr {
    match literal {
        Literal::String(value) | Literal::Date(value) | Literal::Timestamp(value) => {
            SqlExpr::StringLiteral(value.clone())
        }
        Literal::Integer(value) => SqlExpr::Number(value.to_string()),
        Literal::Float(value) => SqlExpr::Number(value.clone()),
        Literal::Bool(value) => SqlExpr::Boolean(*value),
        Literal::Null => SqlExpr::Null,
    }
}
