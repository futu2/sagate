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

fn flatten_apply(expr: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut args = Vec::new();
    let mut current = expr;
    while let Expr::Apply { function, argument } = current {
        args.push(argument.as_ref());
        current = function;
    }
    args.reverse();
    (current, args)
}

fn primitive_value_type(name: &str) -> Option<Type> {
    match name {
        "__table" => Some(Type::Function(
            Box::new(Type::String),
            Box::new(Type::Function(
                Box::new(Type::String),
                Box::new(Type::Relation(Row::default())),
            )),
        )),
        "__where" | "__select" | "__mapKey" | "__mapValue" | "__merge" | "__agg"
        | "__joinInner" | "__joinLeft" | "__joinRight" | "__joinFull" => {
            Some(binary_scalar_type(Type::Any, Type::Any, Type::Any))
        }
        "__snake" => Some(type_from_mapper(MapperType::Known(Mapper::Snake), true)),
        "__kebab" => Some(type_from_mapper(MapperType::Known(Mapper::Kebab), true)),
        "__camel" => Some(type_from_mapper(MapperType::Known(Mapper::Camel), true)),
        "__prefix" | "__suffix" => Some(Type::Function(
            Box::new(Type::String),
            Box::new(type_from_mapper(MapperType::Unknown, true)),
        )),
        "__maybe" => Some(type_from_mapper(MapperType::Known(Mapper::Maybe), false)),
        "__list" => Some(type_from_mapper(MapperType::Known(Mapper::List), false)),
        "__date" => Some(Type::Function(Box::new(Type::String), Box::new(Type::Date))),
        "__timestamp" => Some(Type::Function(
            Box::new(Type::String),
            Box::new(Type::Timestamp),
        )),
        "__add" | "__sub" => Some(overloaded_type(vec![
            binary_scalar_type(Type::Int, Type::Int, Type::Int),
            binary_scalar_type(Type::Float, Type::Float, Type::Float),
        ])),
        "__mul" | "__mod" => Some(binary_scalar_type(Type::Int, Type::Int, Type::Int)),
        "__div" => Some(binary_scalar_type(Type::Float, Type::Float, Type::Float)),
        "__eq" | "__ne" => Some(comparison_type(true)),
        "__lt" | "__le" | "__gt" | "__ge" => Some(comparison_type(false)),
        "__and" | "__or" => Some(binary_scalar_type(Type::Bool, Type::Bool, Type::Bool)),
        "__count" => Some(Type::Function(
            Box::new(Type::Variable(0)),
            Box::new(Type::Aggregate(Box::new(Type::Int))),
        )),
        "__sum" | "__min" | "__max" => Some(Type::Function(
            Box::new(Type::Variable(0)),
            Box::new(Type::Aggregate(Box::new(Type::Variable(0)))),
        )),
        "__avg" => Some(Type::Function(
            Box::new(Type::Variable(0)),
            Box::new(Type::Aggregate(Box::new(Type::Float))),
        )),
        "__group" => Some(Type::Function(
            Box::new(Type::Variable(0)),
            Box::new(Type::Group(Box::new(Type::Variable(0)))),
        )),
        _ => None,
    }
}

fn binary_scalar_type(argument: Type, right: Type, result: Type) -> Type {
    Type::Function(
        Box::new(argument),
        Box::new(Type::Function(Box::new(right), Box::new(result))),
    )
}

fn comparison_type(include_bool: bool) -> Type {
    let mut types = vec![
        Type::Int,
        Type::Float,
        Type::String,
        Type::Date,
        Type::Timestamp,
    ];
    if include_bool {
        types.push(Type::Bool);
    }
    overloaded_type(
        types
            .into_iter()
            .map(|ty| binary_scalar_type(ty.clone(), ty, Type::Bool))
            .collect(),
    )
}

pub(super) fn comparison_operator(operator: &str) -> Option<CompareOp> {
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

fn expect_relation(ty: Type) -> Result<Row, TypeError> {
    relation_row(&ty).ok_or_else(|| TypeError::new(format!("expected a relation, got {ty}")))
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

fn predicate_value(expr: &Expr) -> Result<Predicate, TypeError> {
    match expr {
        Expr::Predicate(predicate) => Ok(predicate.clone()),
        Expr::Lambda { body, .. } => predicate_value_from_expr(body),
        Expr::Annotated { expr, .. } => predicate_value(expr),
        _ => Err(TypeError::new("where expects a predicate")),
    }
}

fn predicate_value_from_expr(expr: &Expr) -> Result<Predicate, TypeError> {
    let (op, left, right) = match expr {
        Expr::Binary { op, left, right } => (op.clone(), left.as_ref(), right.as_ref()),
        Expr::Apply { .. } => {
            let (head, arguments) = flatten_apply(expr);
            if arguments.len() != 2 {
                return Err(TypeError::new("where expects a predicate"));
            }
            let Expr::Var(operator) = head else {
                return Err(TypeError::new("where expects a predicate"));
            };
            let Some(op) = comparison_operator(operator) else {
                return Err(TypeError::new("where expects a comparison predicate"));
            };
            (op, arguments[0], arguments[1])
        }
        _ => return Err(TypeError::new("where expects a predicate")),
    };
    let field = match left {
        Expr::Field(field) => field.clone(),
        Expr::Access { field, .. } => field.clone(),
        _ => return Err(TypeError::new("predicate must compare a row field")),
    };
    let Expr::Literal(value) = right else {
        return Err(TypeError::new(
            "predicate must compare a field with a literal",
        ));
    };
    Ok(Predicate {
        field,
        op,
        value: value.clone(),
    })
}
