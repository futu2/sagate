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

fn check_join_predicate(expr: &Expr, left: &Row, right: &Row) -> Result<(), TypeError> {
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
    let (head, arguments) = flatten_apply(body);
    let Expr::Var(operator) = head else {
        return Err(TypeError::new("join predicate must compare row fields"));
    };
    if comparison_operator(operator).is_none() || arguments.len() != 2 {
        return Err(TypeError::new("join predicate must compare row fields"));
    }
    let Some((first_param, first_field)) = join_field_access(arguments[0]) else {
        return Err(TypeError::new("join predicate must compare row fields"));
    };
    let Some((second_param, second_field)) = join_field_access(arguments[1]) else {
        return Err(TypeError::new("join predicate must compare row fields"));
    };
    let (left_field, right_field) = if first_param == left_param && second_param == right_param {
        (first_field, second_field)
    } else if first_param == right_param && second_param == left_param {
        (second_field, first_field)
    } else {
        return Err(TypeError::new(
            "join predicate must compare left and right rows",
        ));
    };
    let left_type = left.field(left_field).map(|column| &column.ty);
    let right_type = right.field(right_field).map(|column| &column.ty);
    if left_type.is_none() && !left.columns.is_empty() {
        return Err(TypeError::new(format!(
            "unknown field '{left_field}' in left join input"
        )));
    }
    if right_type.is_none() && !right.columns.is_empty() {
        return Err(TypeError::new(format!(
            "unknown field '{right_field}' in right join input"
        )));
    }
    if let (Some(left_type), Some(right_type)) = (left_type, right_type) {
        if !type_compatible(left_type, right_type) {
            return Err(TypeError::new(format!(
                "join compares {left_type} with {right_type}"
            )));
        }
    }
    Ok(())
}

fn join_field_access(expr: &Expr) -> Option<(&str, &str)> {
    let Expr::Access { target, field } = expr else {
        return None;
    };
    let Expr::Var(param) = target.as_ref() else {
        return None;
    };
    Some((param, field))
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

fn select_row(input: &Row, fields: &[SelectField]) -> Result<Row, TypeError> {
    let mut output = Vec::with_capacity(fields.len());
    for field in fields {
        let column = input.field(&field.field);
        if column.is_none() && !input.columns.is_empty() {
            return Err(TypeError::new(format!(
                "unknown field '{}' in select",
                field.field
            )));
        }
        let ty = column.map_or(Type::Any, |column| column.ty.clone());
        output.push(Column {
            name: field.alias.clone(),
            ty,
        });
    }
    Ok(Row::new(output))
}

fn check_predicate(row: &Row, predicate: &Predicate) -> Result<(), TypeError> {
    let Some(column) = row.field(&predicate.field) else {
        if row.columns.is_empty() {
            return Ok(());
        }
        return Err(TypeError::new(format!(
            "unknown field '{}' in where",
            predicate.field
        )));
    };
    let literal_ty = literal_type(&predicate.value);
    if literal_ty != Type::Any
        && column.ty != literal_ty
        && !is_nullable_match(&column.ty, &literal_ty)
    {
        return Err(TypeError::new(format!(
            "cannot compare field '{}' of type {} with {}",
            predicate.field, column.ty, literal_ty
        )));
    }
    Ok(())
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

