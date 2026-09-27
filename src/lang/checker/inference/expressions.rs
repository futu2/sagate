fn infer_expr(
    expr: &Expr,
    environment: &HashMap<String, Type>,
) -> Result<Type, TypeError> {
    // Keep inferred lambda variables separate from variables written in a
    // signature. A binding is inferred in its own HM scope.
    let mut state = InferState {
        next_variable: 1_000_000,
        substitutions: HashMap::new(),
    };
    let inferred = infer_expr_with_state(expr, environment, &mut state)?;
    Ok(resolve_type(inferred, &state.substitutions))
}

fn infer_expr_with_state(
    expr: &Expr,
    environment: &HashMap<String, Type>,
    state: &mut InferState,
) -> Result<Type, TypeError> {
    match expr {
        Expr::Var(name) => environment
            .get(name)
            .cloned()
            .or_else(|| name.starts_with("__").then_some(Type::Any))
            .ok_or_else(|| TypeError::new(format!("unknown variable '{name}'"))),
        Expr::Literal(literal) => Ok(literal_type(literal)),
        Expr::SqlTemplate(_) => Ok(Type::Any),
        Expr::Annotated { expr, ty } => {
            let ty = freshen_type(ty, state);
            let inferred = infer_expr_with_state(expr, environment, state)?;
            apply_annotation(inferred, Some(&ty))
        }
        Expr::Field(_) | Expr::Access { .. } => Ok(Type::Any),
        Expr::RowLiteral(fields) => {
            let mut columns = Vec::with_capacity(fields.len());
            for (name, value) in fields {
                let ty = infer_expr_with_state(value, environment, state)?;
                columns.push(Column {
                    name: name.clone(),
                    ty,
                });
            }
            Ok(Type::Record(Row::new(columns)))
        }
        Expr::List(elements) => {
            let mut element_ty: Option<Type> = None;
            for element in elements {
                let ty = infer_expr_with_state(element, environment, state)?;
                match &element_ty {
                    Some(previous) => unify_types(previous.clone(), ty, state)?,
                    None => element_ty = Some(ty),
                }
            }
            Ok(Type::List(Box::new(element_ty.unwrap_or(Type::Any))))
        }
        Expr::Mapper { mapper, key: true } => Ok(Type::KeyMapperWitness(Box::new(
            MapperType::Known(mapper.clone()),
        ))),
        Expr::Mapper { mapper, key: false } => Ok(Type::ValueMapperWitness(Box::new(
            MapperType::Known(mapper.clone()),
        ))),
        Expr::Overloaded(cases) => {
            let mut types = Vec::with_capacity(cases.len());
            let base_state = state.clone();
            let mut successful_states = Vec::with_capacity(cases.len());
            for case in cases {
                let mut case_state = base_state.clone();
                let expression = case.annotation.as_ref().map_or_else(
                    || (*case.expr).clone(),
                    |annotation| annotate_lambda_parameters((*case.expr).clone(), annotation),
                );
                let inferred =
                    infer_expr_with_state(&expression, environment, &mut case_state)?;
                types.push(apply_annotation(inferred, case.annotation.as_ref())?);
                successful_states.push(case_state);
            }
            if successful_states.len() == 1 {
                *state = successful_states.pop().unwrap();
            }
            Ok(overloaded_type(types))
        }
        Expr::Lambda {
            param,
            annotation,
            body,
        } => {
            let parameter_ty = annotation
                .as_ref()
                .map(|ty| freshen_type(ty, state))
                .unwrap_or_else(|| state.fresh_type());
            let mut scoped = environment.clone();
            scoped.insert(param.clone(), parameter_ty.clone());
            let body_ty = infer_expr_with_state(body, &scoped, state)?;
            Ok(Type::Function(Box::new(parameter_ty), Box::new(body_ty)))
        }
        Expr::Apply { .. } => infer_application(expr, environment, state),
        Expr::Let { name, value, body } => {
            let value_ty = infer_expr_with_state(value, environment, state)?;
            let mut scoped = environment.clone();
            scoped.insert(name.clone(), value_ty);
            infer_expr_with_state(body, &scoped, state)
        }
    }
}
fn infer_application(
    expr: &Expr,
    environment: &HashMap<String, Type>,
    state: &mut InferState,
) -> Result<Type, TypeError> {
    let (head, arguments) = flatten_apply(expr);
    if let Expr::Lambda { param, body, .. } = head {
        if arguments.is_empty() {
            return Ok(Type::Function(
                Box::new(Type::Variable(0)),
                Box::new(Type::Any),
            ));
        }
        let mut reduced = Expr::Lambda {
            param: param.clone(),
            annotation: None,
            body: body.clone(),
        };
        for argument in arguments {
            reduced = match reduced {
                Expr::Lambda { param, body, .. } => substitute(&body, &param, argument),
                function => Expr::Apply {
                    function: Box::new(function),
                    argument: Box::new(argument.clone()),
                },
            };
        }
        return infer_expr_with_state(&reduced, environment, state);
    }
    if let Expr::Var(name) = head {
        match name.as_str() {
            "__table" => {
                if arguments.len() != 2 {
                    return Ok(Type::Any);
                }
                for argument in &arguments {
                    let argument_ty = infer_expr_with_state(argument, environment, state)?;
                    if !matches!(argument_ty, Type::String | Type::Any | Type::Variable(_)) {
                        return Err(TypeError::new("table expects a schema name and table name"));
                    }
                }
                // The row is intentionally open: its fields come from the
                // database catalog, while later row operations can refine the
                // visible fields used by a query.
                return Ok(Type::Relation(Row::default()));
            }
            "__where" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let row_expr = relation_row_expr(relation_ty, state)?;
                let row = row_expr.normalize().unwrap_or_default();
                let mut predicate_ty =
                    infer_expr_with_state(arguments[0], environment, state)?;
                if matches!(predicate_ty, Type::Variable(_)) {
                    let expected = Type::Function(
                        Box::new(type_from_bare_row_expr(row_expr.clone())),
                        Box::new(Type::Bool),
                    );
                    unify_types(predicate_ty.clone(), expected, state)?;
                    predicate_ty = resolve_type(predicate_ty, &state.substitutions);
                }
                if let Type::Function(input, output) = predicate_ty {
                    unify_types(*output, Type::Bool, state)?;
                    unify_types(*input, type_from_bare_row_expr(row_expr.clone()), state)?;
                    if row_expr.normalize().is_some() && !matches!(arguments[0], Expr::Var(_)) {
                        check_row_expression(arguments[0], &row)?;
                    }
                } else {
                    return Err(TypeError::new(format!(
                        "where expects a row-to-bool function, got {predicate_ty}"
                    )));
                }
                return Ok(type_from_row_expr(row_expr));
            }
            "__select" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let input_row = relation_row_expr(relation_ty, state)?;
                let mut projection_ty =
                    infer_expr_with_state(arguments[0], environment, state)?;
                if matches!(projection_ty, Type::Variable(_)) {
                    let expected = Type::Function(
                        Box::new(type_from_bare_row_expr(input_row.clone())),
                        Box::new(type_from_bare_row_expr(RowExpr::Variable(fresh_id(state)))),
                    );
                    unify_types(projection_ty.clone(), expected, state)?;
                    projection_ty = resolve_type(projection_ty, &state.substitutions);
                }
                let Type::Function(input, output) = projection_ty else {
                    return Err(TypeError::new(format!(
                        "select expects a row-to-row function, got {projection_ty}"
                    )));
                };
                unify_types(*input, type_from_bare_row_expr(input_row.clone()), state)?;
                let visible_input = input_row.normalize().unwrap_or_default();
                if let Some((param, fields)) = row_literal_fields(arguments[0]) {
                    let mut scope = HashMap::new();
                    scope.insert(param, &visible_input);
                    let mut columns = Vec::with_capacity(fields.len());
                    for (name, value) in fields {
                        // Named row functions are validated where they are
                        // defined; inline values are checked against the row.
                        let ty = if matches!(value, Expr::Var(_)) {
                            Type::Any
                        } else {
                            row_expression_type(value, &scope)?
                        };
                        columns.push(Column {
                            name: name.clone(),
                            ty,
                        });
                    }
                    let selected = Row::new(columns);
                    unify_types(*output, Type::Record(selected.clone()), state)?;
                    return Ok(Type::Relation(selected));
                }
                return relation_of_row_type(resolve_type(*output, &state.substitutions))
                    .ok_or_else(|| TypeError::new("select result must be a row"));
            }
            "__mapKey" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let Some(row) = row_expr_from_type(&relation_ty) else {
                    return Err(TypeError::new(format!(
                        "expected a relation, got {relation_ty}"
                    )));
                };
                let mapper_ty = infer_expr_with_state(arguments[0], environment, state)?;
                let mapper = mapper_value(arguments[0])
                    .map(MapperType::Known)
                    .or_else(|_| match mapper_ty {
                        Type::KeyMapperWitness(mapper) => Ok(*mapper),
                        Type::Variable(id) => Ok(MapperType::Variable(id)),
                        Type::KeyMapper => Ok(MapperType::Unknown),
                        other => Err(TypeError::new(format!(
                            "mapKey expects a key mapper, got {other}"
                        ))),
                    })?;
                return Ok(type_from_row_expr(RowExpr::MapKey(
                    Box::new(mapper),
                    Box::new(row),
                )));
            }
            "__mapValue" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let Some(row) = row_expr_from_type(&relation_ty) else {
                    return Err(TypeError::new(format!(
                        "expected a relation, got {relation_ty}"
                    )));
                };
                let mapper_ty = infer_expr_with_state(arguments[0], environment, state)?;
                let mapper = mapper_value(arguments[0])
                    .map(MapperType::Known)
                    .or_else(|_| match mapper_ty {
                        Type::ValueMapperWitness(mapper) => Ok(*mapper),
                        Type::Variable(id) => Ok(MapperType::Variable(id)),
                        Type::ValueMapper => Ok(MapperType::Unknown),
                        other => Err(TypeError::new(format!(
                            "mapValue expects a value mapper, got {other}"
                        ))),
                    })?;
                return Ok(type_from_row_expr(RowExpr::MapValue(
                    Box::new(mapper),
                    Box::new(row),
                )));
            }
            "__agg" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let input_row = relation_row_expr(relation_ty, state)?;
                let mut projection_ty =
                    infer_expr_with_state(arguments[0], environment, state)?;
                if matches!(projection_ty, Type::Variable(_)) {
                    let expected = Type::Function(
                        Box::new(type_from_bare_row_expr(input_row.clone())),
                        Box::new(type_from_bare_row_expr(RowExpr::Variable(fresh_id(state)))),
                    );
                    unify_types(projection_ty.clone(), expected, state)?;
                    projection_ty = resolve_type(projection_ty, &state.substitutions);
                }
                let Type::Function(input, output) = projection_ty else {
                    return Err(TypeError::new(format!(
                        "agg expects a row-to-row function, got {projection_ty}"
                    )));
                };
                unify_types(*input, type_from_bare_row_expr(input_row.clone()), state)?;
                let visible_input = input_row.normalize().unwrap_or_default();
                if let Some(extracted) = aggregate_fields(arguments[0]) {
                    let fields = extracted?;
                    // The aggregate result is authoritative; the lambda's
                    // inferred body still carries the raw constructor types.
                    let result = aggregate_row(&visible_input, &fields)?;
                    return Ok(Type::Relation(result));
                }
                return relation_of_row_type(resolve_type(*output, &state.substitutions))
                    .ok_or_else(|| TypeError::new("agg result must be a row"));
            }
            "__joinInner" | "__joinLeft" | "__joinRight" | "__joinFull" => {
                if arguments.len() != 3 {
                    return Ok(Type::Function(
                        Box::new(Type::Any),
                        Box::new(Type::Relation(Row::default())),
                    ));
                }
                let right = match infer_expr_with_state(arguments[0], environment, state)? {
                    Type::Relation(row) => row,
                    Type::Any | Type::Variable(_) | Type::RelationVariable(_) => Row::default(),
                    other => {
                        return Err(TypeError::new(format!("expected a relation, got {other}")))
                    }
                };
                let left = match infer_expr_with_state(arguments[2], environment, state)? {
                    Type::Relation(row) => row,
                    Type::Any | Type::Variable(_) | Type::RelationVariable(_) => Row::default(),
                    other => {
                        return Err(TypeError::new(format!("expected a relation, got {other}")))
                    }
                };
                let _ = infer_expr_with_state(arguments[1], environment, state)?;
                if matches!(arguments[1], Expr::Lambda { .. }) {
                    check_join_row_expression(arguments[1], &left, &right)?;
                }
                return Ok(Type::Relation(Row::joined(&left, &right, name)));
            }
            "__merge" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let older_ty = infer_expr_with_state(arguments[0], environment, state)?;
                let newer_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let Some(older) = row_expr_from_type(&older_ty) else {
                    return Err(TypeError::new(format!(
                        "expected a relation, got {older_ty}"
                    )));
                };
                let Some(newer) = row_expr_from_type(&newer_ty) else {
                    return Err(TypeError::new(format!(
                        "expected a relation, got {newer_ty}"
                    )));
                };
                return Ok(type_from_row_expr(RowExpr::Merge(
                    Box::new(older),
                    Box::new(newer),
                )));
            }
            "__asc" | "__desc" => {
                if arguments.len() != 1 {
                    return Ok(Type::Direction);
                }
                let value_ty = infer_expr_with_state(arguments[0], environment, state)?;
                if !is_orderable_type(&value_ty) {
                    return Err(TypeError::new(format!(
                        "order keys must be orderable values (int, float, string, bool, date, or timestamp), got {value_ty}"
                    )));
                }
                return Ok(Type::Direction);
            }
            "__order" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let row_expr = relation_row_expr(relation_ty, state)?;
                let row = row_expr.normalize().unwrap_or_default();
                let mut keys_ty = infer_expr_with_state(arguments[0], environment, state)?;
                if matches!(keys_ty, Type::Variable(_)) {
                    let expected = Type::Function(
                        Box::new(type_from_bare_row_expr(row_expr.clone())),
                        Box::new(Type::List(Box::new(Type::Direction))),
                    );
                    unify_types(keys_ty.clone(), expected, state)?;
                    keys_ty = resolve_type(keys_ty, &state.substitutions);
                }
                let Type::Function(input, output) = keys_ty else {
                    return Err(TypeError::new(format!(
                        "order expects a row-to-list-of-directions function, got {keys_ty}"
                    )));
                };
                unify_types(*input, type_from_bare_row_expr(row_expr.clone()), state)?;
                if let Type::List(element) = resolve_type(*output, &state.substitutions) {
                    if !matches!(
                        element.as_ref(),
                        Type::Direction | Type::Any | Type::Variable(_)
                    ) {
                        return Err(TypeError::new(format!(
                            "order keys must be asc or desc values, got list of {element}"
                        )));
                    }
                }
                if row_expr.normalize().is_some() && !matches!(arguments[0], Expr::Var(_)) {
                    check_row_expression(arguments[0], &row)?;
                }
                return Ok(type_from_row_expr(row_expr));
            }
            "__limit" => {
                if arguments.len() != 2 {
                    return Ok(Type::Function(Box::new(Type::Any), Box::new(Type::Any)));
                }
                match arguments[0] {
                    Expr::Literal(Literal::Integer(count)) if *count >= 0 => {}
                    // A named count is only validated where its definition is
                    // visible; the SQL lowerer requires a literal there.
                    Expr::Var(_) => {}
                    _ => {
                        return Err(TypeError::new(
                            "limit expects a non-negative integer literal",
                        ))
                    }
                }
                let relation_ty = infer_expr_with_state(arguments[1], environment, state)?;
                let row_expr = relation_row_expr(relation_ty, state)?;
                return Ok(type_from_row_expr(row_expr));
            }
            _ => {}
        }
    }
    let (function, argument) = match expr {
        Expr::Apply { function, argument } => (function, argument),
        _ => unreachable!(),
    };
    let function_ty = infer_expr_with_state(function, environment, state)?;
    let argument_ty = infer_expr_with_state(argument, environment, state)?;
    match function_ty {
        Type::Overloaded(candidates) => {
            let mut matches = Vec::new();
            for candidate in candidates {
                let mut candidate_state = state.clone();
                if let Ok(result) =
                    apply_function_type(candidate, &argument_ty, &mut candidate_state)
                {
                    *state = candidate_state;
                    matches.push(result);
                }
            }
            if matches.is_empty() {
                return Err(TypeError::new(format!(
                    "no overload accepts argument of type {argument_ty}"
                )));
            }
            Ok(overloaded_type(matches))
        }
        function @ Type::Function(_, _) => apply_function_type(function, &argument_ty, state),
        Type::Any | Type::Variable(_) => Ok(Type::Any),
        other => Err(TypeError::new(format!(
            "cannot apply value of type {other}"
        ))),
    }
}

/// The row literal behind a select projection: either the literal itself or
/// a single-row lambda whose body is one. The lambda parameter names the row
/// its field expressions read from.
fn row_literal_fields(expr: &Expr) -> Option<(&str, &Vec<(String, Expr)>)> {
    match expr {
        Expr::RowLiteral(fields) => Some(("row", fields)),
        Expr::Lambda { param, body, .. } => match body.as_ref() {
            Expr::RowLiteral(fields) => Some((param.as_str(), fields)),
            _ => None,
        },
        _ => None,
    }
}

pub fn substitute(expr: &Expr, name: &str, replacement: &Expr) -> Expr {
    match expr {
        Expr::Var(variable) if variable == name => replacement.clone(),
        Expr::Apply { function, argument } => Expr::Apply {
            function: Box::new(substitute(function, name, replacement)),
            argument: Box::new(substitute(argument, name, replacement)),
        },
        Expr::Lambda {
            param,
            annotation,
            body,
        } if param != name => Expr::Lambda {
            param: param.clone(),
            annotation: annotation.clone(),
            body: Box::new(substitute(body, name, replacement)),
        },
        Expr::Let {
            name: binding,
            value,
            body,
        } if binding != name => Expr::Let {
            name: binding.clone(),
            value: Box::new(substitute(value, name, replacement)),
            body: Box::new(substitute(body, name, replacement)),
        },
        Expr::Annotated { expr, ty } => Expr::Annotated {
            expr: Box::new(substitute(expr, name, replacement)),
            ty: ty.clone(),
        },
        Expr::Access { target, field } => Expr::Access {
            target: Box::new(substitute(target, name, replacement)),
            field: field.clone(),
        },
        Expr::List(elements) => Expr::List(
            elements
                .iter()
                .map(|element| substitute(element, name, replacement))
                .collect(),
        ),
        Expr::RowLiteral(fields) => Expr::RowLiteral(
            fields
                .iter()
                .map(|(field, value)| (field.clone(), substitute(value, name, replacement)))
                .collect(),
        ),
        Expr::Overloaded(cases) => Expr::Overloaded(
            cases
                .iter()
                .map(|case| OverloadCase {
                    annotation: case.annotation.clone(),
                    expr: Box::new(substitute(&case.expr, name, replacement)),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}
