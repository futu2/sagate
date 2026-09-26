fn predicate_value(expr: &Expr, definitions: &HashMap<String, &Expr>) -> Result<Predicate, String> {
    match expr {
        Expr::Predicate(predicate) => Ok(predicate.clone()),
        Expr::Lambda { body, .. } => predicate_value_from_expr(body),
        Expr::Annotated { expr, .. } => predicate_value(expr, definitions),
        Expr::Var(name) => definitions
            .get(name)
            .ok_or_else(|| format!("unknown predicate '{name}'"))
            .and_then(|value| predicate_value(value, definitions)),
        _ => Err("where expects a predicate".to_owned()),
    }
}

fn predicate_value_from_expr(expr: &Expr) -> Result<Predicate, String> {
    let (op, left, right) = match expr {
        Expr::Binary { op, left, right } => (op.clone(), left.as_ref(), right.as_ref()),
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            if arguments.len() != 2 {
                return Err("where expects a predicate".to_owned());
            }
            let Expr::Var(operator) = head else {
                return Err("where expects a predicate".to_owned());
            };
            let Some(op) = comparison_operator(operator) else {
                return Err("where expects a comparison predicate".to_owned());
            };
            (op, arguments[0], arguments[1])
        }
        _ => return Err("where expects a predicate".to_owned()),
    };
    let field = match left {
        Expr::Field(field) => field.clone(),
        Expr::Access { field, .. } => field.clone(),
        _ => return Err("predicate must compare a row field".to_owned()),
    };
    let value = predicate_literal(right)
        .ok_or_else(|| "predicate must compare a field with a literal".to_owned())?;
    Ok(Predicate {
        field,
        op,
        value,
    })
}

fn predicate_literal(expr: &Expr) -> Option<Literal> {
    match expr {
        Expr::Literal(value) => Some(value.clone()),
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            let head = match head {
                Expr::Annotated { expr, .. } => expr.as_ref(),
                head => head,
            };
            let name = match head {
                Expr::Var(name) => name.as_str(),
                Expr::Lambda { body, .. } => {
                    let Expr::Apply { function, .. } = body.as_ref() else { return None; };
                    let Expr::Var(name) = function.as_ref() else { return None; };
                    name.as_str()
                }
                _ => return None,
            };
            let [Expr::Literal(Literal::String(value))] = arguments.as_slice() else { return None; };
            match name {
                "date" | "__date" => Some(Literal::Date(value.clone())),
                "timestamp" | "__timestamp" => Some(Literal::Timestamp(value.clone())),
                _ => None,
            }
        }
        _ => None,
    }
}

fn comparison_operator(operator: &str) -> Option<CompareOp> {
    match operator {
        "__eq" => Some(CompareOp::Eq),
        "__ne" => Some(CompareOp::Ne),
        "__lt" => Some(CompareOp::Lt),
        "__le" => Some(CompareOp::Le),
        "__gt" => Some(CompareOp::Gt),
        "__ge" => Some(CompareOp::Ge),
        _ => None,
    }
}

fn projection_value(expr: &Expr) -> Result<&Vec<SelectField>, String> {
    match expr {
        Expr::Projection(fields) => Ok(fields),
        Expr::Lambda { body, .. } => match body.as_ref() {
            Expr::Projection(fields) => Ok(fields),
            _ => Err("select expects a projection".to_owned()),
        },
        _ => Err("select expects a projection".to_owned()),
    }
}

fn aggregate_projection(expr: &Expr) -> Result<&Vec<AggregateField>, String> {
    match expr {
        Expr::AggregateProjection(fields) => Ok(fields),
        _ => Err("agg expects an aggregate projection".to_owned()),
    }
}

fn join_predicate_value(
    expr: &Expr,
    definitions: &HashMap<String, &Expr>,
) -> Result<JoinPredicate, String> {
    if let Expr::Var(name) = expr {
        let definition = definitions
            .get(name)
            .ok_or_else(|| format!("unknown join predicate '{name}'"))?;
        return join_predicate_value(definition, definitions);
    }
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
    let (op, left, right) = comparison_parts(body)?;
    let (left_access_param, left_field) = access_part(left)?;
    let (right_access_param, right_field) = access_part(right)?;
    if left_access_param == *left_param && right_access_param == *right_param {
        return Ok(JoinPredicate {
            left_field,
            right_field,
            op,
        });
    }
    if left_access_param == *right_param && right_access_param == *left_param {
        return Ok(JoinPredicate {
            left_field: right_field,
            right_field: left_field,
            op: reverse_compare_operator(op),
        });
    }
    Err("join predicate must compare the left and right rows".to_owned())
}

fn reverse_compare_operator(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Named(name) => CompareOp::Named(name),
        CompareOp::Eq => CompareOp::Eq,
        CompareOp::Ne => CompareOp::Ne,
        CompareOp::Lt => CompareOp::Gt,
        CompareOp::Le => CompareOp::Ge,
        CompareOp::Gt => CompareOp::Lt,
        CompareOp::Ge => CompareOp::Le,
    }
}

fn comparison_parts(expr: &Expr) -> Result<(CompareOp, &Expr, &Expr), String> {
    match expr {
        Expr::Binary { op, left, right } => Ok((op.clone(), left, right)),
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            if arguments.len() != 2 {
                return Err("join predicate must be a comparison".to_owned());
            }
            let Expr::Var(operator) = head else {
                return Err("join predicate must be a comparison".to_owned());
            };
            let op = comparison_operator(operator)
                .ok_or_else(|| "join predicate must be a comparison".to_owned())?;
            Ok((op, arguments[0], arguments[1]))
        }
        _ => Err("join predicate must be a comparison".to_owned()),
    }
}

fn access_part(expr: &Expr) -> Result<(String, String), String> {
    let Expr::Access { target, field } = expr else {
        return Err("join predicate must compare row fields".to_owned());
    };
    let Expr::Var(parameter) = target.as_ref() else {
        return Err("join predicate must compare row fields".to_owned());
    };
    Ok((parameter.clone(), field.clone()))
}

fn mapper_value(
    expr: &Expr,
    key: bool,
    definitions: &HashMap<String, &Expr>,
) -> Result<Mapper, String> {
    match expr {
        Expr::Mapper {
            mapper,
            key: actual,
        } if *actual == key => Ok(mapper.clone()),
        Expr::Lambda { param, body, .. } if matches!(body.as_ref(), Expr::Var(name) if name == param) => {
            Ok(Mapper::Identity)
        }
        Expr::Var(name) if definitions.contains_key(name) => {
            mapper_value(definitions[name], key, definitions)
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
                return mapper_value(&reduced, key, definitions);
            }
            if let Expr::Var(name) = head {
                if let Some(definition) = definitions.get(name) {
                    let mut expanded = (*definition).clone();
                    for argument in arguments {
                        expanded = Expr::Apply {
                            function: Box::new(expanded),
                            argument: Box::new(argument.clone()),
                        };
                    }
                    return mapper_value(&expanded, key, definitions);
                }
            }
            match (head, arguments.as_slice()) {
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if key && name == "__prefix" =>
                {
                    Ok(Mapper::Prefix(value.clone()))
                }
                (Expr::Var(name), [Expr::Literal(Literal::String(value))])
                    if key && name == "__suffix" =>
                {
                    Ok(Mapper::Suffix(value.clone()))
                }
                _ if key => Err("mapKey expects a key mapper".to_owned()),
                _ => Err("mapValue expects a value mapper".to_owned()),
            }
        }
        Expr::Var(name) if name == "__snake" => Ok(Mapper::Snake),
        Expr::Var(name) if name == "__kebab" => Ok(Mapper::Kebab),
        Expr::Var(name) if name == "__camel" => Ok(Mapper::Camel),
        Expr::Var(name) if name == "__maybe" => Ok(Mapper::Maybe),
        Expr::Var(name) if name == "__list" => Ok(Mapper::List),
        _ if key => Err("mapKey expects a key mapper".to_owned()),
        _ => Err("mapValue expects a value mapper".to_owned()),
    }
}

fn compile_predicate(
    predicate: &Predicate,
    row: &Row,
    alias: &str,
) -> Result<SqlExpr, CompileError> {
    if row.field(&predicate.field).is_none() && !row.columns.is_empty() {
        return Err(CompileError::new(format!(
            "unknown field '{}' in where",
            predicate.field
        )));
    }
    if matches!(predicate.value, Literal::Null) {
        let negated = match predicate.op {
            CompareOp::Eq => false,
            CompareOp::Ne => true,
            _ => return Err(CompileError::new("null can only be compared with == or !=")),
        };
        return Ok(SqlExpr::IsNull {
            expr: Box::new(sql_predicate_column(&predicate.field, alias)),
            negated,
        });
    }
    Ok(SqlExpr::BinaryOp {
        left: Box::new(sql_predicate_column(&predicate.field, alias)),
        op: compare_operator(&predicate.op)?,
        right: Box::new(sql_predicate_literal(&predicate.value)),
    })
}

fn compare_operator(operator: &CompareOp) -> Result<BinaryOperator, CompileError> {
    Ok(match operator {
        CompareOp::Named(_) => return Err(CompileError::new("unresolved comparison operator")),
        CompareOp::Eq => BinaryOperator::Eq,
        CompareOp::Ne => BinaryOperator::Neq,
        CompareOp::Lt => BinaryOperator::Lt,
        CompareOp::Le => BinaryOperator::LtEq,
        CompareOp::Gt => BinaryOperator::Gt,
        CompareOp::Ge => BinaryOperator::GtEq,
    })
}

fn sql_predicate_column(name: &str, table: &str) -> SqlExpr {
    SqlExpr::Column {
        table: Some(table.to_owned()),
        name: name.to_owned(),
        quote_style: QuoteStyle::DoubleQuote,
        table_quote_style: if table == "q" {
            QuoteStyle::None
        } else {
            QuoteStyle::DoubleQuote
        },
    }
}

fn sql_predicate_literal(literal: &Literal) -> SqlExpr {
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

pub(super) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
