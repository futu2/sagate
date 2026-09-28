#[derive(Clone)]
struct InferState {
    next_variable: u32,
    substitutions: HashMap<VariableKey, Type>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum VariableKey {
    Value(u32),
    Row(u32),
    Mapper(MapperAxis, u32),
}

impl InferState {
    fn fresh_type(&mut self) -> Type {
        let variable = Type::Variable(self.next_variable);
        self.next_variable = self.next_variable.saturating_add(1);
        variable
    }
}

/// Instantiate the variables written in one source annotation. Annotation
/// variables are schemes, not global mutable metavariables: every use gets a
/// fresh value, row, and mapper variable. This is the HM rule that prevents
/// `id 1` and `id true` from sharing a unification cell.
fn freshen_type(ty: &Type, state: &mut InferState) -> Type {
    let mut values = HashMap::new();
    let mut rows = HashMap::new();
    let mut key_mappers = HashMap::new();
    let mut value_mappers = HashMap::new();
    freshen_type_with_maps(
        ty,
        state,
        &mut values,
        &mut rows,
        &mut key_mappers,
        &mut value_mappers,
    )
}

fn fresh_id(state: &mut InferState) -> u32 {
    let id = state.next_variable;
    state.next_variable = state.next_variable.saturating_add(1);
    id
}

fn freshen_type_with_maps(
    ty: &Type,
    state: &mut InferState,
    values: &mut HashMap<u32, u32>,
    rows: &mut HashMap<u32, u32>,
    key_mappers: &mut HashMap<u32, u32>,
    value_mappers: &mut HashMap<u32, u32>,
) -> Type {
    let value = |id: u32, state: &mut InferState, map: &mut HashMap<u32, u32>| {
        *map.entry(id).or_insert_with(|| fresh_id(state))
    };
    let row = |id: u32, state: &mut InferState, map: &mut HashMap<u32, u32>| {
        *map.entry(id).or_insert_with(|| fresh_id(state))
    };
    match ty {
        Type::Variable(id) => Type::Variable(value(*id, state, values)),
        Type::RelationExpr(expr) => Type::RelationExpr(Box::new(freshen_row_expr(
            expr,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::RowType(expr) => Type::RowType(Box::new(freshen_row_expr(
            expr,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::RowExpression(expr) => Type::RowExpression(Box::new(freshen_row_expr(
            expr,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::Record(row) => Type::Record(Row {
            columns: row
                .columns
                .iter()
                .map(|column| Column {
                    name: column.name.clone(),
                    ty: freshen_type_with_maps(
                        &column.ty,
                        state,
                        values,
                        rows,
                        key_mappers,
                        value_mappers,
                    ),
                })
                .collect(),
            extent: row.extent,
        }),
        Type::Maybe(inner) => Type::Maybe(Box::new(freshen_type_with_maps(
            inner,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::List(inner) => Type::List(Box::new(freshen_type_with_maps(
            inner,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::Aggregate(inner) => Type::Aggregate(Box::new(freshen_type_with_maps(
            inner,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::Group(inner) => Type::Group(Box::new(freshen_type_with_maps(
            inner,
            state,
            values,
            rows,
            key_mappers,
            value_mappers,
        ))),
        Type::RowVariable(id) => Type::RowVariable(row(*id, state, rows)),
        Type::RelationVariable(id) => Type::RelationVariable(row(*id, state, rows)),
        Type::KeyMapperOf(input, output) => Type::KeyMapperOf(
            Box::new(freshen_type_with_maps(
                input,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
            Box::new(freshen_type_with_maps(
                output,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
        Type::ValueMapperOf(input, output) => Type::ValueMapperOf(
            Box::new(freshen_type_with_maps(
                input,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
            Box::new(freshen_type_with_maps(
                output,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
        Type::KeyMapperWitness(mapper) => {
            Type::KeyMapperWitness(Box::new(freshen_mapper_type(mapper, state, key_mappers)))
        }
        Type::ValueMapperWitness(mapper) => {
            Type::ValueMapperWitness(Box::new(freshen_mapper_type(mapper, state, value_mappers)))
        }
        Type::Function(argument, result) => Type::Function(
            Box::new(freshen_type_with_maps(
                argument,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
            Box::new(freshen_type_with_maps(
                result,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
        Type::Overloaded(types) => Type::Overloaded(
            types
                .iter()
                .map(|ty| {
                    freshen_type_with_maps(ty, state, values, rows, key_mappers, value_mappers)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn freshen_mapper_type(
    mapper: &MapperType,
    state: &mut InferState,
    variables: &mut HashMap<u32, u32>,
) -> MapperType {
    match mapper {
        MapperType::Variable(id) => {
            MapperType::Variable(*variables.entry(*id).or_insert_with(|| fresh_id(state)))
        }
        other => other.clone(),
    }
}

fn freshen_row_expr(
    expr: &RowExpr,
    state: &mut InferState,
    values: &mut HashMap<u32, u32>,
    rows: &mut HashMap<u32, u32>,
    key_mappers: &mut HashMap<u32, u32>,
    value_mappers: &mut HashMap<u32, u32>,
) -> RowExpr {
    match expr {
        RowExpr::Concrete(row) => RowExpr::Concrete(Row {
            columns: row
                .columns
                .iter()
                .map(|column| Column {
                    name: column.name.clone(),
                    ty: freshen_type_with_maps(
                        &column.ty,
                        state,
                        values,
                        rows,
                        key_mappers,
                        value_mappers,
                    ),
                })
                .collect(),
            extent: row.extent,
        }),
        RowExpr::Variable(id) => {
            RowExpr::Variable(*rows.entry(*id).or_insert_with(|| fresh_id(state)))
        }
        RowExpr::Merge(older, newer) => RowExpr::Merge(
            Box::new(freshen_row_expr(
                older,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
            Box::new(freshen_row_expr(
                newer,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
        RowExpr::MapKey(mapper, row) => RowExpr::MapKey(
            Box::new(freshen_mapper_type(mapper, state, key_mappers)),
            Box::new(freshen_row_expr(
                row,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
        RowExpr::MapValue(mapper, row) => RowExpr::MapValue(
            Box::new(freshen_mapper_type(mapper, state, value_mappers)),
            Box::new(freshen_row_expr(
                row,
                state,
                values,
                rows,
                key_mappers,
                value_mappers,
            )),
        ),
    }
}

fn resolve_type(ty: Type, substitutions: &HashMap<VariableKey, Type>) -> Type {
    fn resolve_row_variable(
        id: u32,
        query: bool,
        substitutions: &HashMap<VariableKey, Type>,
        seen: &mut Vec<VariableKey>,
    ) -> Type {
        let key = VariableKey::Row(id);
        if seen.contains(&key) {
            return if query {
                Type::RelationVariable(id)
            } else {
                Type::RowVariable(id)
            };
        }
        let Some(replacement) = substitutions.get(&key) else {
            return if query {
                Type::RelationVariable(id)
            } else {
                Type::RowVariable(id)
            };
        };
        seen.push(key);
        let replacement = go(replacement.clone(), substitutions, seen);
        seen.pop();
        match replacement {
            Type::Record(row) | Type::Relation(row) => {
                if query {
                    Type::Relation(row)
                } else {
                    Type::Record(row)
                }
            }
            Type::RowType(row) | Type::RowExpression(row) | Type::RelationExpr(row) => {
                if query {
                    type_from_row_expr(*row)
                } else {
                    type_from_bare_row_expr(*row)
                }
            }
            Type::RelationVariable(id) | Type::RowVariable(id) => {
                if query {
                    Type::RelationVariable(id)
                } else {
                    Type::RowVariable(id)
                }
            }
            other => other,
        }
    }

    fn resolve_row(
        row: Row,
        substitutions: &HashMap<VariableKey, Type>,
        seen: &mut Vec<VariableKey>,
    ) -> Row {
        Row {
            columns: row
                .columns
                .into_iter()
                .map(|column| Column {
                    name: column.name,
                    ty: go(column.ty, substitutions, seen),
                })
                .collect(),
            extent: row.extent,
        }
    }

    fn go(
        ty: Type,
        substitutions: &HashMap<VariableKey, Type>,
        seen: &mut Vec<VariableKey>,
    ) -> Type {
        match ty {
            Type::Variable(id) => {
                let key = VariableKey::Value(id);
                if seen.contains(&key) {
                    return Type::Variable(id);
                }
                if let Some(replacement) = substitutions.get(&key) {
                    seen.push(key);
                    let resolved = go(replacement.clone(), substitutions, seen);
                    seen.pop();
                    resolved
                } else {
                    Type::Variable(id)
                }
            }
            Type::RelationVariable(id) => resolve_row_variable(id, true, substitutions, seen),
            Type::RowVariable(id) => resolve_row_variable(id, false, substitutions, seen),
            Type::Relation(row) => Type::Relation(resolve_row(row, substitutions, seen)),
            Type::Record(row) => Type::Record(resolve_row(row, substitutions, seen)),
            Type::RowType(row) | Type::RowExpression(row) => {
                type_from_bare_row_expr(resolve_row_expr(*row, substitutions, seen))
            }
            Type::RelationExpr(row) => {
                type_from_row_expr(resolve_row_expr(*row, substitutions, seen))
            }
            Type::Maybe(inner) => Type::Maybe(Box::new(go(*inner, substitutions, seen))),
            Type::List(inner) => Type::List(Box::new(go(*inner, substitutions, seen))),
            Type::Aggregate(inner) => Type::Aggregate(Box::new(go(*inner, substitutions, seen))),
            Type::Group(inner) => Type::Group(Box::new(go(*inner, substitutions, seen))),
            Type::KeyMapperOf(input, output) => Type::KeyMapperOf(
                Box::new(go(*input, substitutions, seen)),
                Box::new(go(*output, substitutions, seen)),
            ),
            Type::ValueMapperOf(input, output) => Type::ValueMapperOf(
                Box::new(go(*input, substitutions, seen)),
                Box::new(go(*output, substitutions, seen)),
            ),
            Type::KeyMapperWitness(mapper) => Type::KeyMapperWitness(Box::new(
                resolve_mapper_type(*mapper, MapperAxis::Key, substitutions),
            )),
            Type::ValueMapperWitness(mapper) => Type::ValueMapperWitness(Box::new(
                resolve_mapper_type(*mapper, MapperAxis::Value, substitutions),
            )),
            Type::Function(argument, result) => Type::Function(
                Box::new(go(*argument, substitutions, seen)),
                Box::new(go(*result, substitutions, seen)),
            ),
            Type::Overloaded(types) => Type::Overloaded(
                types
                    .into_iter()
                    .map(|ty| go(ty, substitutions, seen))
                    .collect(),
            ),
            other => other,
        }
    }
    go(ty, substitutions, &mut Vec::new())
}

fn resolve_row_expr(
    row: RowExpr,
    substitutions: &HashMap<VariableKey, Type>,
    seen: &mut Vec<VariableKey>,
) -> RowExpr {
    match row {
        RowExpr::Concrete(row) => RowExpr::Concrete(Row {
            columns: row
                .columns
                .into_iter()
                .map(|column| Column {
                    name: column.name,
                    ty: resolve_type(column.ty, substitutions),
                })
                .collect(),
            extent: row.extent,
        }),
        RowExpr::Variable(id) => match substitutions.get(&VariableKey::Row(id)) {
            Some(Type::Relation(replacement)) | Some(Type::Record(replacement)) => {
                RowExpr::Concrete(replacement.clone())
            }
            Some(Type::RowExpression(replacement)) => {
                resolve_row_expr((**replacement).clone(), substitutions, seen)
            }
            Some(Type::RelationExpr(replacement)) => {
                resolve_row_expr((**replacement).clone(), substitutions, seen)
            }
            Some(Type::RelationVariable(replacement)) if !seen.contains(&VariableKey::Row(id)) => {
                seen.push(VariableKey::Row(id));
                let result = resolve_row_expr(RowExpr::Variable(*replacement), substitutions, seen);
                seen.pop();
                result
            }
            Some(Type::RowVariable(replacement)) if !seen.contains(&VariableKey::Row(id)) => {
                seen.push(VariableKey::Row(id));
                let result = resolve_row_expr(RowExpr::Variable(*replacement), substitutions, seen);
                seen.pop();
                result
            }
            _ => RowExpr::Variable(id),
        },
        RowExpr::Merge(older, newer) => RowExpr::Merge(
            Box::new(resolve_row_expr(*older, substitutions, seen)),
            Box::new(resolve_row_expr(*newer, substitutions, seen)),
        ),
        RowExpr::MapKey(mapper, row) => RowExpr::MapKey(
            Box::new(resolve_mapper_type(*mapper, MapperAxis::Key, substitutions)),
            Box::new(resolve_row_expr(*row, substitutions, seen)),
        ),
        RowExpr::MapValue(mapper, row) => RowExpr::MapValue(
            Box::new(resolve_mapper_type(
                *mapper,
                MapperAxis::Value,
                substitutions,
            )),
            Box::new(resolve_row_expr(*row, substitutions, seen)),
        ),
    }
}

fn resolve_mapper_type(
    mapper: MapperType,
    axis: MapperAxis,
    substitutions: &HashMap<VariableKey, Type>,
) -> MapperType {
    match mapper {
        MapperType::Variable(id) => match substitutions.get(&VariableKey::Mapper(axis, id)) {
            Some(Type::KeyMapperWitness(value)) | Some(Type::ValueMapperWitness(value)) => {
                (**value).clone()
            }
            _ => MapperType::Variable(id),
        },
        other => other,
    }
}

fn unify_types(actual: Type, expected: Type, state: &mut InferState) -> Result<(), TypeError> {
    let actual = resolve_type(actual, &state.substitutions);
    let expected = resolve_type(expected, &state.substitutions);
    if actual == expected || matches!(actual, Type::Any) || matches!(expected, Type::Any) {
        return Ok(());
    }
    match (actual, expected) {
        (Type::Variable(id), ty) => bind_variable(VariableKey::Value(id), ty, state),
        (ty, Type::Variable(id)) => bind_variable(VariableKey::Value(id), ty, state),
        (Type::RelationVariable(id), ty) => bind_variable(VariableKey::Row(id), ty, state),
        (ty, Type::RelationVariable(id)) => bind_variable(VariableKey::Row(id), ty, state),
        (Type::RowVariable(id), Type::RowType(row) | Type::RowExpression(row))
        | (Type::RowType(row) | Type::RowExpression(row), Type::RowVariable(id)) => {
            unify_row_expr(RowExpr::Variable(id), *row, state)
        }
        (Type::RowVariable(id), Type::Record(row)) | (Type::Record(row), Type::RowVariable(id)) => {
            unify_row_expr(RowExpr::Variable(id), RowExpr::Concrete(row), state)
        }
        (Type::RowVariable(id), ty) => bind_variable(VariableKey::Row(id), ty, state),
        (ty, Type::RowVariable(id)) => bind_variable(VariableKey::Row(id), ty, state),
        (Type::KeyMapperWitness(actual), Type::KeyMapperWitness(expected)) => {
            unify_mapper(*actual, *expected, MapperAxis::Key, state)
        }
        (Type::ValueMapperWitness(actual), Type::ValueMapperWitness(expected)) => {
            unify_mapper(*actual, *expected, MapperAxis::Value, state)
        }
        (
            Type::Function(actual_arg, actual_result),
            Type::Function(expected_arg, expected_result),
        ) => {
            unify_types(*actual_arg, *expected_arg, state)?;
            unify_types(*actual_result, *expected_result, state)
        }
        (Type::Record(actual), Type::Record(expected)) => unify_rows(&actual, &expected, state),
        (Type::Maybe(actual), Type::Maybe(expected))
        | (Type::List(actual), Type::List(expected))
        | (Type::Aggregate(actual), Type::Aggregate(expected))
        | (Type::Group(actual), Type::Group(expected)) => unify_types(*actual, *expected, state),
        (Type::Relation(actual), Type::Relation(expected)) => unify_rows(&actual, &expected, state),
        (Type::RelationExpr(actual), Type::RelationExpr(expected)) => {
            unify_row_expr(*actual, *expected, state)
        }
        (Type::RowType(actual), Type::RowType(expected))
        | (Type::RowExpression(actual), Type::RowExpression(expected)) => {
            unify_row_expr(*actual, *expected, state)
        }
        (Type::Record(actual), Type::RowType(expected))
        | (Type::RowType(expected), Type::Record(actual)) => {
            unify_row_expr(*expected, RowExpr::Concrete(actual), state)
        }
        (Type::Record(actual), Type::RowExpression(expected))
        | (Type::RowExpression(expected), Type::Record(actual)) => {
            unify_row_expr(*expected, RowExpr::Concrete(actual), state)
        }
        (Type::Relation(actual), Type::RelationExpr(expected))
        | (Type::RelationExpr(expected), Type::Relation(actual)) => {
            unify_row_expr(*expected, RowExpr::Concrete(actual), state)
        }
        (actual, expected) => {
            if type_compatible(&actual, &expected) {
                Ok(())
            } else {
                Err(TypeError::new(format!(
                    "cannot unify {actual} with {expected}"
                )))
            }
        }
    }
}

fn bind_variable(key: VariableKey, ty: Type, state: &mut InferState) -> Result<(), TypeError> {
    if matches!(ty, Type::Variable(id) if key == VariableKey::Value(id))
        || matches!(ty, Type::RelationVariable(id) | Type::RowVariable(id) if key == VariableKey::Row(id))
        || matches!(ty, Type::RowType(ref row) | Type::RowExpression(ref row) | Type::RelationExpr(ref row)
            if matches!(row.as_ref(), RowExpr::Variable(id) if key == VariableKey::Row(*id)))
    {
        return Ok(());
    }
    if occurs_in(key, &ty, &state.substitutions) {
        return Err(TypeError::new("infinite type"));
    }
    state.substitutions.insert(key, ty);
    Ok(())
}

fn occurs_in(key: VariableKey, ty: &Type, substitutions: &HashMap<VariableKey, Type>) -> bool {
    let resolved = resolve_type(ty.clone(), substitutions);
    match resolved {
        Type::Variable(id) => key == VariableKey::Value(id),
        Type::RelationVariable(id) | Type::RowVariable(id) => key == VariableKey::Row(id),
        Type::Maybe(inner) | Type::List(inner) | Type::Aggregate(inner) | Type::Group(inner) => {
            occurs_in(key, &inner, substitutions)
        }
        Type::Function(argument, result) => {
            occurs_in(key, &argument, substitutions) || occurs_in(key, &result, substitutions)
        }
        Type::RelationExpr(row) | Type::RowType(row) | Type::RowExpression(row) => {
            occurs_in_row(key, &row)
        }
        Type::Record(row) => row
            .columns
            .iter()
            .any(|column| occurs_in(key, &column.ty, substitutions)),
        _ => false,
    }
}

fn occurs_in_row(key: VariableKey, row: &RowExpr) -> bool {
    match row {
        RowExpr::Variable(id) => key == VariableKey::Row(*id),
        RowExpr::Merge(older, newer) => occurs_in_row(key, older) || occurs_in_row(key, newer),
        RowExpr::MapKey(_, row) | RowExpr::MapValue(_, row) => occurs_in_row(key, row),
        RowExpr::Concrete(_) => false,
    }
}

fn unify_mapper(
    actual: MapperType,
    expected: MapperType,
    axis: MapperAxis,
    state: &mut InferState,
) -> Result<(), TypeError> {
    match (actual, expected) {
        (MapperType::Known(actual), MapperType::Known(expected)) if actual == expected => Ok(()),
        (MapperType::Unknown, _) | (_, MapperType::Unknown) => Ok(()),
        (MapperType::Variable(id), mapper) | (mapper, MapperType::Variable(id)) => {
            let replacement = match axis {
                MapperAxis::Key => Type::KeyMapperWitness(Box::new(mapper)),
                MapperAxis::Value => Type::ValueMapperWitness(Box::new(mapper)),
            };
            bind_variable(VariableKey::Mapper(axis, id), replacement, state)
        }
        (actual, expected) => Err(TypeError::new(format!(
            "cannot unify mapper {actual} with {expected}"
        ))),
    }
}

fn unify_rows(actual: &Row, expected: &Row, state: &mut InferState) -> Result<(), TypeError> {
    if actual.columns.is_empty() && actual.extent == Extent::Open {
        return Ok(());
    }
    if expected.columns.is_empty() && expected.extent == Extent::Open {
        return Ok(());
    }
    if actual.extent == Extent::Closed
        && expected.extent == Extent::Closed
        && actual.columns.len() != expected.columns.len()
    {
        return Err(TypeError::new("closed rows have different field counts"));
    }
    for actual_column in &actual.columns {
        let Some(expected_column) = expected.field(&actual_column.name) else {
            if expected.extent == Extent::Open {
                continue;
            }
            return Err(TypeError::new(format!(
                "closed row is missing field '{}'",
                actual_column.name
            )));
        };
        unify_types(actual_column.ty.clone(), expected_column.ty.clone(), state)?;
    }
    for expected_column in &expected.columns {
        if actual.field(&expected_column.name).is_none() && actual.extent == Extent::Closed {
            return Err(TypeError::new(format!(
                "closed row is missing field '{}'",
                expected_column.name
            )));
        }
    }
    Ok(())
}

fn unify_row_expr(
    actual: RowExpr,
    expected: RowExpr,
    state: &mut InferState,
) -> Result<(), TypeError> {
    match (actual, expected) {
        (RowExpr::Variable(id), row) | (row, RowExpr::Variable(id)) => {
            bind_variable(VariableKey::Row(id), type_from_row_expr(row), state)
        }
        (RowExpr::Concrete(actual), RowExpr::Concrete(expected)) => {
            unify_rows(&actual, &expected, state)
        }
        (RowExpr::Merge(a_old, a_new), RowExpr::Merge(e_old, e_new)) => {
            unify_row_expr(*a_old, *e_old, state)?;
            unify_row_expr(*a_new, *e_new, state)
        }
        (RowExpr::MapKey(a_mapper, a_row), RowExpr::MapKey(e_mapper, e_row)) => {
            unify_mapper(*a_mapper, *e_mapper, MapperAxis::Key, state)?;
            unify_row_expr(*a_row, *e_row, state)
        }
        (RowExpr::MapValue(a_mapper, a_row), RowExpr::MapValue(e_mapper, e_row)) => {
            unify_mapper(*a_mapper, *e_mapper, MapperAxis::Value, state)?;
            unify_row_expr(*a_row, *e_row, state)
        }
        (actual, expected) => Err(TypeError::new(format!(
            "cannot unify row expression {actual} with {expected}"
        ))),
    }
}

