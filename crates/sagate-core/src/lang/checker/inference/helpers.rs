#[derive(Clone, Copy)]
enum TypeVariableKind {
    Value,
    Row,
    Mapper(MapperAxis),
}

fn substitute_type(ty: Type, variable: u32, replacement: &Type, kind: TypeVariableKind) -> Type {
    match ty {
        Type::Variable(id) if matches!(kind, TypeVariableKind::Value) && id == variable => {
            replacement.clone()
        }
        Type::RelationVariable(id) if matches!(kind, TypeVariableKind::Row) && id == variable => {
            replacement.clone()
        }
        Type::RowVariable(id) if matches!(kind, TypeVariableKind::Row) && id == variable => {
            replacement.clone()
        }
        Type::RelationExpr(row) => {
            type_from_row_expr(substitute_row_expr(*row, variable, replacement, kind))
        }
        Type::RowType(row) => {
            type_from_bare_row_expr(substitute_row_expr(*row, variable, replacement, kind))
        }
        Type::KeyMapperWitness(mapper)
            if matches!(kind, TypeVariableKind::Mapper(MapperAxis::Key))
                && matches!(*mapper, MapperType::Variable(id) if id == variable) =>
        {
            replacement.clone()
        }
        Type::ValueMapperWitness(mapper)
            if matches!(kind, TypeVariableKind::Mapper(MapperAxis::Value))
                && matches!(*mapper, MapperType::Variable(id) if id == variable) =>
        {
            replacement.clone()
        }
        Type::Maybe(inner) => Type::Maybe(Box::new(substitute_type(
            *inner,
            variable,
            replacement,
            kind,
        ))),
        Type::List(inner) => Type::List(Box::new(substitute_type(
            *inner,
            variable,
            replacement,
            kind,
        ))),
        Type::Aggregate(inner) => Type::Aggregate(Box::new(substitute_type(
            *inner,
            variable,
            replacement,
            kind,
        ))),
        Type::Group(inner) => Type::Group(Box::new(substitute_type(
            *inner,
            variable,
            replacement,
            kind,
        ))),
        Type::KeyMapperOf(input, output) => Type::KeyMapperOf(
            Box::new(substitute_type(*input, variable, replacement, kind)),
            Box::new(substitute_type(*output, variable, replacement, kind)),
        ),
        Type::ValueMapperOf(input, output) => Type::ValueMapperOf(
            Box::new(substitute_type(*input, variable, replacement, kind)),
            Box::new(substitute_type(*output, variable, replacement, kind)),
        ),
        Type::Overloaded(types) => Type::Overloaded(
            types
                .into_iter()
                .map(|ty| substitute_type(ty, variable, replacement, kind))
                .collect(),
        ),
        Type::Function(argument, result) => Type::Function(
            Box::new(substitute_type(*argument, variable, replacement, kind)),
            Box::new(substitute_type(*result, variable, replacement, kind)),
        ),
        other => other,
    }
}

fn substitute_row_expr(
    row: RowExpr,
    variable: u32,
    replacement: &Type,
    kind: TypeVariableKind,
) -> RowExpr {
    match row {
        RowExpr::Variable(id) if matches!(kind, TypeVariableKind::Row) && id == variable => {
            row_expr_from_any_row_type(replacement).unwrap_or(RowExpr::Variable(id))
        }
        RowExpr::Merge(older, newer) => RowExpr::Merge(
            Box::new(substitute_row_expr(*older, variable, replacement, kind)),
            Box::new(substitute_row_expr(*newer, variable, replacement, kind)),
        ),
        RowExpr::MapKey(mapper, row) => RowExpr::MapKey(
            Box::new(substitute_mapper_type(*mapper, variable, replacement, kind)),
            Box::new(substitute_row_expr(*row, variable, replacement, kind)),
        ),
        RowExpr::MapValue(mapper, row) => RowExpr::MapValue(
            Box::new(substitute_mapper_type(*mapper, variable, replacement, kind)),
            Box::new(substitute_row_expr(*row, variable, replacement, kind)),
        ),
        other => other,
    }
}

fn substitute_mapper_type(
    mapper: MapperType,
    variable: u32,
    replacement: &Type,
    kind: TypeVariableKind,
) -> MapperType {
    if matches!(kind, TypeVariableKind::Mapper(_))
        && matches!(mapper, MapperType::Variable(id) if id == variable)
    {
        match replacement {
            Type::KeyMapperWitness(value) | Type::ValueMapperWitness(value) => (**value).clone(),
            Type::KeyMapper | Type::ValueMapper => MapperType::Unknown,
            _ => mapper,
        }
    } else {
        mapper
    }
}

fn overloaded_type(types: Vec<Type>) -> Type {
    let mut flattened = Vec::new();
    for ty in types {
        match ty {
            Type::Overloaded(nested) => flattened.extend(nested),
            ty => flattened.push(ty),
        }
    }
    flattened.dedup();
    match flattened.as_slice() {
        [] => Type::Any,
        [only] => only.clone(),
        _ => Type::Overloaded(flattened),
    }
}

fn apply_function_type(
    function: Type,
    argument: &Type,
    state: &mut InferState,
) -> Result<Type, TypeError> {
    match &function {
        Type::Function(expected, _) if matches!(expected.as_ref(), Type::KeyMapperWitness(_)) => {
            if let Type::KeyMapperWitness(mapper) = expected.as_ref() {
                if let MapperType::Variable(id) = **mapper {
                    if mapper_type_from_type(argument).is_some() {
                        if let Type::Function(_, result) = &function {
                            let result = substitute_type(
                                (**result).clone(),
                                id,
                                argument,
                                TypeVariableKind::Mapper(MapperAxis::Key),
                            );
                            unify_mapper(
                                MapperType::Variable(id),
                                mapper_type_from_type(argument).unwrap_or(MapperType::Unknown),
                                MapperAxis::Key,
                                state,
                            )?;
                            return Ok(result);
                        }
                    }
                }
            }
        }
        Type::Function(expected, _) if matches!(expected.as_ref(), Type::ValueMapperWitness(_)) => {
            if let Type::ValueMapperWitness(mapper) = expected.as_ref() {
                if let MapperType::Variable(id) = **mapper {
                    if value_mapper_type_from_type(argument).is_some() {
                        if let Type::Function(_, result) = &function {
                            let result = substitute_type(
                                (**result).clone(),
                                id,
                                argument,
                                TypeVariableKind::Mapper(MapperAxis::Value),
                            );
                            unify_mapper(
                                MapperType::Variable(id),
                                value_mapper_type_from_type(argument)
                                    .unwrap_or(MapperType::Unknown),
                                MapperAxis::Value,
                                state,
                            )?;
                            return Ok(result);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    match function {
        Type::Function(expected, result) if matches!(*expected, Type::Variable(_)) => {
            let Type::Variable(id) = *expected else {
                unreachable!()
            };
            let result = substitute_type(*result, id, argument, TypeVariableKind::Value);
            unify_types(Type::Variable(id), argument.clone(), state)?;
            Ok(result)
        }
        Type::Function(expected, result) if matches!(*expected, Type::RelationVariable(_)) => {
            let Type::RelationVariable(id) = *expected else {
                unreachable!()
            };
            match argument {
                Type::Relation(_)
                | Type::RelationExpr(_)
                | Type::RelationVariable(_)
                | Type::Any
                | Type::Variable(_) => Ok(substitute_type(
                    *result,
                    id,
                    argument,
                    TypeVariableKind::Row,
                )),
                _ => Err(TypeError::new(format!(
                    "expected a relation, got {argument}"
                ))),
            }
        }
        Type::Function(expected, result)
            if matches!(expected.as_ref(), Type::KeyMapperWitness(_)) =>
        {
            let Type::KeyMapperWitness(mapper) = expected.as_ref() else {
                unreachable!()
            };
            let MapperType::Variable(id) = **mapper else {
                return Err(TypeError::new(format!(
                    "expected argument of type {expected}, got {argument}"
                )));
            };
            if mapper_type_from_type(argument).is_none() {
                return Err(TypeError::new(format!(
                    "expected a key mapper, got {argument}"
                )));
            }
            let result = substitute_type(
                *result,
                id,
                argument,
                TypeVariableKind::Mapper(MapperAxis::Key),
            );
            unify_mapper(
                MapperType::Variable(id),
                mapper_type_from_type(argument).unwrap_or(MapperType::Unknown),
                MapperAxis::Key,
                state,
            )?;
            Ok(result)
        }
        Type::Function(expected, result)
            if matches!(expected.as_ref(), Type::ValueMapperWitness(_)) =>
        {
            let Type::ValueMapperWitness(mapper) = expected.as_ref() else {
                unreachable!()
            };
            let MapperType::Variable(id) = **mapper else {
                return Err(TypeError::new(format!(
                    "expected argument of type {expected}, got {argument}"
                )));
            };
            if value_mapper_type_from_type(argument).is_none() {
                return Err(TypeError::new(format!(
                    "expected a value mapper, got {argument}"
                )));
            }
            let result = substitute_type(
                *result,
                id,
                argument,
                TypeVariableKind::Mapper(MapperAxis::Value),
            );
            unify_mapper(
                MapperType::Variable(id),
                value_mapper_type_from_type(argument).unwrap_or(MapperType::Unknown),
                MapperAxis::Value,
                state,
            )?;
            Ok(result)
        }
        Type::Function(expected, result) => {
            unify_types(argument.clone(), *expected, state)?;
            Ok(*result)
        }
        Type::Any | Type::Variable(_) => Ok(Type::Any),
        other => Err(TypeError::new(format!(
            "cannot apply value of type {other}"
        ))),
    }
}

pub fn flatten_apply(expr: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut args = Vec::new();
    let mut current = expr;
    while let Expr::Apply { function, argument } = current {
        args.push(argument.as_ref());
        current = function;
    }
    args.reverse();
    (current, args)
}

fn relation_row_expr(ty: Type, state: &mut InferState) -> Result<RowExpr, TypeError> {
    if let Some(row) = row_expr_from_type(&ty) {
        return Ok(row);
    }
    if matches!(ty, Type::Variable(_)) {
        let row = RowExpr::Variable(fresh_id(state));
        unify_types(ty, type_from_row_expr(row.clone()), state)?;
        return Ok(row);
    }
    if matches!(ty, Type::Any) {
        return Ok(RowExpr::Concrete(Row::default()));
    }
    Err(TypeError::new(format!("expected a relation, got {ty}")))
}
