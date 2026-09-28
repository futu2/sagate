

#[derive(Clone)]
struct Definition {
    expr: Expr,
    annotation: Option<Type>,
}

// ---------- HM-shaped type checking -------------------------------------

pub fn type_check(program: &Program) -> Result<HashMap<String, Row>, TypeError> {
    let foreign = foreign_declarations(&program.bindings);
    let mut environment = HashMap::<String, Type>::new();
    let mut definitions = HashMap::<String, Definition>::new();
    let mut query_rows = HashMap::new();
    for binding in &program.bindings {
        if is_sql_template_definition(&binding.expr)
            && !has_sql_template_function_signature(&binding.expr, binding.annotation.as_ref())
        {
            return Err(TypeError::new("SQL template requires a function type signature")
                .at_definition(&binding.name, "definition"));
        }
        if let Some(annotation) = &binding.annotation {
            validate_type_kinds(annotation).map_err(|error| {
                TypeError::new(error.to_string())
                    .at_definition(&binding.name, "kind checking")
            })?;
        }
        let expanded = expand_aliases(&binding.expr, &definitions, &environment, &foreign);
        if let Some(annotation) = &binding.annotation {
            check_expr_against(&binding.expr, annotation, &environment, &foreign).map_err(
                |error| error.at_definition(&binding.name, "signature"),
            )?;
        }
        let inferred = if let Some(annotation) = &binding.annotation {
            if annotation_is_row_polymorphic(annotation) {
                annotation.clone()
            } else {
                infer_expr(&expanded, &environment, &foreign).map_err(|error| {
                    error.at_definition(&binding.name, "inference")
                })?
            }
        } else {
            infer_expr(&expanded, &environment, &foreign).map_err(|error| {
                error.at_definition(&binding.name, "inference")
            })?
        };
        let ty = apply_annotation(inferred, binding.annotation.as_ref()).map_err(|error| {
            error.at_definition(&binding.name, "annotation")
        })?;
        if let Some(row) = relation_row(&ty) {
            query_rows.insert(binding.name.clone(), row);
        }
        environment.insert(binding.name.clone(), ty);
        definitions.insert(
            binding.name.clone(),
            Definition {
                expr: binding.expr.clone(),
                annotation: binding.annotation.clone(),
            },
        );
    }
    Ok(query_rows)
}

fn validate_type_kinds(ty: &Type) -> Result<Kind, TypeError> {
    let mut variable_kinds = HashMap::new();
    validate_type_kinds_with(ty, &mut variable_kinds)
}

fn constrain_variable_kind(
    id: u32,
    kind: Kind,
    variable_kinds: &mut HashMap<u32, Kind>,
) -> Result<(), TypeError> {
    if let Some(existing) = variable_kinds.insert(id, kind) {
        if existing != kind {
            return Err(TypeError::new(format!(
                "type variable {} is used at kinds {existing:?} and {kind:?}",
                type_variable_name(id)
            )));
        }
    }
    Ok(())
}

fn require_type_kind(ty: &Type, variable_kinds: &mut HashMap<u32, Kind>) -> Result<(), TypeError> {
    let kind = validate_type_kinds_with(ty, variable_kinds)?;
    if kind != Kind::Type {
        return Err(TypeError::new(format!(
            "function and type constructors require a Type, got {kind:?}"
        )));
    }
    Ok(())
}

fn validate_row_expr_kinds(
    row: &RowExpr,
    variable_kinds: &mut HashMap<u32, Kind>,
) -> Result<(), TypeError> {
    match row {
        RowExpr::Concrete(row) => {
            for column in &row.columns {
                require_type_kind(&column.ty, variable_kinds)?;
            }
        }
        RowExpr::Variable(id) => constrain_variable_kind(*id, Kind::Row, variable_kinds)?,
        RowExpr::Merge(older, newer) => {
            validate_row_expr_kinds(older, variable_kinds)?;
            validate_row_expr_kinds(newer, variable_kinds)?;
        }
        RowExpr::MapKey(mapper, row) => {
            validate_row_expr_kinds(row, variable_kinds)?;
            mapper
                .kind(MapperAxis::Key)
                .ok_or_else(|| TypeError::new("mapkey expects a key mapper witness"))?;
            if let MapperType::Variable(id) = mapper.as_ref() {
                constrain_variable_kind(*id, Kind::KeyMapper, variable_kinds)?;
            }
        }
        RowExpr::MapValue(mapper, row) => {
            validate_row_expr_kinds(row, variable_kinds)?;
            mapper
                .kind(MapperAxis::Value)
                .ok_or_else(|| TypeError::new("mapvalue expects a value mapper witness"))?;
            if let MapperType::Variable(id) = mapper.as_ref() {
                constrain_variable_kind(*id, Kind::ValueMapper, variable_kinds)?;
            }
        }
    }
    Ok(())
}

fn validate_type_kinds_with(
    ty: &Type,
    variable_kinds: &mut HashMap<u32, Kind>,
) -> Result<Kind, TypeError> {
    match ty {
        Type::RelationExpr(row) | Type::RowType(row) => {
            validate_row_expr_kinds(row, variable_kinds)?;
            Ok(Kind::Type)
        }
        Type::RowExpression(row) => {
            validate_row_expr_kinds(row, variable_kinds)?;
            Ok(Kind::Row)
        }
        Type::Record(row) => {
            for column in &row.columns {
                require_type_kind(&column.ty, variable_kinds)?;
            }
            Ok(Kind::Type)
        }
        Type::Relation(_) | Type::KeyMapper | Type::ValueMapper => Ok(Kind::Type),
        Type::Variable(id) => {
            constrain_variable_kind(*id, Kind::Type, variable_kinds)?;
            Ok(Kind::Type)
        }
        Type::RelationVariable(id) | Type::RowVariable(id) => {
            constrain_variable_kind(*id, Kind::Row, variable_kinds)?;
            Ok(Kind::Row)
        }
        Type::KeyMapperWitness(mapper) => {
            mapper
                .kind(MapperAxis::Key)
                .ok_or_else(|| TypeError::new("keymapper witness has a value mapper"))?;
            if let MapperType::Variable(id) = mapper.as_ref() {
                constrain_variable_kind(*id, Kind::KeyMapper, variable_kinds)?;
            }
            Ok(Kind::Type)
        }
        Type::ValueMapperWitness(mapper) => {
            mapper
                .kind(MapperAxis::Value)
                .ok_or_else(|| TypeError::new("valuemapper witness has a key mapper"))?;
            if let MapperType::Variable(id) = mapper.as_ref() {
                constrain_variable_kind(*id, Kind::ValueMapper, variable_kinds)?;
            }
            Ok(Kind::Type)
        }
        Type::Maybe(inner) | Type::List(inner) | Type::Aggregate(inner) | Type::Group(inner) => {
            require_type_kind(inner, variable_kinds)?;
            Ok(Kind::Type)
        }
        Type::KeyMapperOf(input, output) | Type::ValueMapperOf(input, output) => {
            require_type_kind(input, variable_kinds)?;
            require_type_kind(output, variable_kinds)?;
            Ok(Kind::Type)
        }
        Type::Function(argument, result) => {
            require_type_kind(argument, variable_kinds)?;
            require_type_kind(result, variable_kinds)?;
            Ok(Kind::Type)
        }
        Type::Overloaded(types) => {
            for ty in types {
                require_type_kind(ty, variable_kinds)?;
            }
            Ok(Kind::Type)
        }
        Type::Int
        | Type::String
        | Type::Bool
        | Type::Float
        | Type::Date
        | Type::Timestamp
        | Type::Any
        | Type::Direction => Ok(Kind::Type),
    }
}

fn check_expr_against(
    expr: &Expr,
    expected: &Type,
    environment: &HashMap<String, Type>,
    foreign: &ForeignOps,
) -> Result<(), TypeError> {
    match (expr, expected) {
        (Expr::Lambda { param, body, .. }, Type::Function(argument, result)) => {
            let mut scoped = environment.clone();
            scoped.insert(param.clone(), (**argument).clone());
            check_expr_against(body, result, &scoped, foreign)
        }
        (Expr::Overloaded(cases), _) => {
            for case in cases {
                let case_type = case.annotation.as_ref().unwrap_or(expected);
                check_expr_against(&case.expr, case_type, environment, foreign)?;
            }
            Ok(())
        }
        _ => {
            let inferred = infer_expr(expr, environment, foreign)?;
            if type_compatible(&inferred, expected) {
                Ok(())
            } else {
                Err(TypeError::new(format!(
                    "type annotation {expected} does not match inferred type {inferred}"
                )))
            }
        }    }
}

fn apply_annotation(inferred: Type, annotation: Option<&Type>) -> Result<Type, TypeError> {
    let Some(annotation) = annotation else {
        return Ok(inferred);
    };
    if type_compatible(&inferred, annotation) {
        match (&inferred, annotation) {
            // An empty relation row is an open row annotation. Keep the
            // concrete fields inferred from the expression instead of
            // erasing them when applying the annotation.
            (Type::Relation(actual), Type::Relation(expected)) if expected.columns.is_empty() => {
                Ok(Type::Relation(actual.clone()))
            }
            (Type::Relation(actual), Type::RelationExpr(expected))
                if expected.normalize().is_none() =>
            {
                Ok(Type::Relation(actual.clone()))
            }
            (Type::KeyMapperWitness(_), Type::KeyMapper)
            | (Type::ValueMapperWitness(_), Type::ValueMapper) => Ok(inferred),
            _ => Ok(annotation.clone()),
        }
    } else {
        Err(TypeError::new(format!(
            "type annotation {annotation} does not match inferred type {inferred}"
        )))
    }
}

fn type_compatible(inferred: &Type, expected: &Type) -> bool {
    match (inferred, expected) {
        (Type::Any, _) | (_, Type::Any) | (Type::Variable(_), _) | (_, Type::Variable(_)) => true,
        (Type::Overloaded(actual), expected) => actual
            .iter()
            .any(|candidate| type_compatible(candidate, expected)),
        (actual, Type::Overloaded(expected)) => expected
            .iter()
            .any(|candidate| type_compatible(actual, candidate)),
        (Type::Relation(actual), Type::Relation(expected)) => rows_compatible(actual, expected),
        (Type::RelationExpr(actual), Type::RelationExpr(expected)) => {
            match (actual.normalize(), expected.normalize()) {
                (Some(actual), Some(expected)) => {
                    type_compatible(&Type::Relation(actual), &Type::Relation(expected))
                }
                _ => actual == expected,
            }
        }
        (Type::RowType(actual), Type::RowType(expected))
        | (Type::RowExpression(actual), Type::RowExpression(expected)) => {
            match (actual.normalize(), expected.normalize()) {
                (Some(actual), Some(expected)) => {
                    type_compatible(&Type::Record(actual), &Type::Record(expected))
                }
                _ => actual == expected,
            }
        }
        (Type::Record(actual), Type::RowType(expected))
        | (Type::RowType(expected), Type::Record(actual)) => {
            expected.normalize().is_none_or(|expected| {
                type_compatible(&Type::Record(actual.clone()), &Type::Record(expected))
            })
        }
        (Type::Record(actual), Type::Record(expected)) => rows_compatible(actual, expected),
        (Type::Relation(actual), Type::RelationExpr(expected))
        | (Type::RelationExpr(expected), Type::Relation(actual)) => {
            expected.normalize().is_none_or(|expected| {
                type_compatible(&Type::Relation(actual.clone()), &Type::Relation(expected))
            })
        }
        (Type::RowVariable(_), Type::RowVariable(_))
        | (Type::RowVariable(_), Type::Record(_))
        | (Type::Record(_), Type::RowVariable(_))
        | (Type::RowVariable(_), Type::RowType(_))
        | (Type::RowType(_), Type::RowVariable(_))
        | (Type::RowVariable(_), Type::RowExpression(_))
        | (Type::RowExpression(_), Type::RowVariable(_)) => true,
        (Type::KeyMapper, Type::KeyMapperOf(_, _)) | (Type::KeyMapperOf(_, _), Type::KeyMapper) => {
            true
        }
        (Type::KeyMapper, Type::KeyMapperWitness(_))
        | (Type::KeyMapperWitness(_), Type::KeyMapper) => true,
        (Type::KeyMapperWitness(actual), Type::KeyMapperWitness(expected)) => {
            actual == expected
                || matches!(**actual, MapperType::Unknown)
                || matches!(**expected, MapperType::Unknown)
        }
        (
            Type::KeyMapperOf(actual_input, actual_output),
            Type::KeyMapperOf(expected_input, expected_output),
        ) => {
            type_compatible(actual_input, expected_input)
                && type_compatible(actual_output, expected_output)
        }
        (Type::ValueMapper, Type::ValueMapperOf(_, _))
        | (Type::ValueMapperOf(_, _), Type::ValueMapper) => true,
        (Type::ValueMapper, Type::ValueMapperWitness(_))
        | (Type::ValueMapperWitness(_), Type::ValueMapper) => true,
        (Type::ValueMapperWitness(actual), Type::ValueMapperWitness(expected)) => {
            actual == expected
                || matches!(**actual, MapperType::Unknown)
                || matches!(**expected, MapperType::Unknown)
        }
        (
            Type::ValueMapperOf(actual_input, actual_output),
            Type::ValueMapperOf(expected_input, expected_output),
        ) => {
            type_compatible(actual_input, expected_input)
                && type_compatible(actual_output, expected_output)
        }
        (Type::Maybe(actual), Type::Maybe(expected))
        | (Type::List(actual), Type::List(expected))
        | (Type::Aggregate(actual), Type::Aggregate(expected))
        | (Type::Group(actual), Type::Group(expected)) => type_compatible(actual, expected),
        (
            Type::Function(actual_arg, actual_result),
            Type::Function(expected_arg, expected_result),
        ) => {
            type_compatible(actual_arg, expected_arg)
                && type_compatible(actual_result, expected_result)
        }
        (actual, expected) => actual == expected,
    }
}

fn rows_compatible(actual: &Row, expected: &Row) -> bool {
    expected.columns.is_empty()
        || (actual.columns.is_empty() && actual.extent == Extent::Open)
        || (actual.extent == Extent::Open
            && expected.columns.iter().all(|expected| {
                actual
                    .field(&expected.name)
                    .is_some_and(|actual| type_compatible(&actual.ty, &expected.ty))
            }))
        || (actual.extent == Extent::Closed
            && expected.extent == Extent::Closed
            && actual.columns.len() == expected.columns.len()
            && actual.columns.iter().all(|actual| {
                expected
                    .field(&actual.name)
                    .is_some_and(|expected| type_compatible(&actual.ty, &expected.ty))
            }))
}

/// Inline known top-level values before inference. This is the small amount
/// of elaboration needed for first-class prelude functions such as
/// `let active = where (.ok == true); ... & active`: after expansion the
/// ordinary application rule sees the prelude head directly.
fn expand_aliases(
    expr: &Expr,
    definitions: &HashMap<String, Definition>,
    environment: &HashMap<String, Type>,
    foreign: &ForeignOps,
) -> Expr {
    fn go(
        expr: &Expr,
        definitions: &HashMap<String, Definition>,
        environment: &HashMap<String, Type>,
        foreign: &ForeignOps,
        stack: &mut Vec<String>,
        bound: &mut Vec<String>,
    ) -> Expr {
        match expr {
            Expr::Var(name)
                if definitions.contains_key(name)
                    && !bound.contains(name)
                    // Foreign declarations are the backend call surface: their
                    // applications are typed by the foreign rules, so never
                    // inline them.
                    && !foreign.contains_key(name)
                    && !is_sql_template_definition(&definitions[name].expr)
                    && !matches!(environment.get(name), Some(Type::Relation(_)))
                    && !stack.contains(name) =>
            {
                stack.push(name.clone());
                let definition = &definitions[name];
                let expanded = go(
                    &definition.expr,
                    definitions,
                    environment,
                    foreign,
                    stack,
                    bound,
                );
                stack.pop();
                match &definition.annotation {
                    Some(annotation) if !annotation_is_row_polymorphic(annotation) => {
                        Expr::Annotated {
                            expr: Box::new(expanded),
                            ty: annotation.clone(),
                        }
                    }
                    // Preserve the quantified row kinds on lambda parameters
                    // while exposing the body for concrete row operations.
                    Some(annotation) => annotate_lambda_parameters(expanded, annotation),
                    None => expanded,
                }
            }
            Expr::Apply { function, argument } => Expr::Apply {
                function: Box::new(go(function, definitions, environment, foreign, stack, bound)),
                argument: Box::new(go(argument, definitions, environment, foreign, stack, bound)),
            },
            Expr::Lambda {
                param,
                annotation,
                body,
            } => {
                bound.push(param.clone());
                let result = Expr::Lambda {
                    param: param.clone(),
                    annotation: annotation.clone(),
                    body: Box::new(go(body, definitions, environment, foreign, stack, bound)),
                };
                bound.pop();
                result
            }
            Expr::Let { name, value, body } => {
                let value = go(value, definitions, environment, foreign, stack, bound);
                bound.push(name.clone());
                let body = go(body, definitions, environment, foreign, stack, bound);
                bound.pop();
                Expr::Let {
                    name: name.clone(),
                    value: Box::new(value),
                    body: Box::new(body),
                }
            }
            Expr::Annotated { expr, ty } => Expr::Annotated {
                expr: Box::new(go(expr, definitions, environment, foreign, stack, bound)),
                ty: ty.clone(),
            },
            Expr::Access { target, field } => Expr::Access {
                target: Box::new(go(target, definitions, environment, foreign, stack, bound)),
                field: field.clone(),
            },
            Expr::List(elements) => Expr::List(
                elements
                    .iter()
                    .map(|element| go(element, definitions, environment, foreign, stack, bound))
                    .collect(),
            ),
            // Row literal values expand so that user-aliased aggregate
            // constructors (`countRows = count; agg {all_rows = countRows}`)
            // reach their foreign heads for the relational rules.
            Expr::RowLiteral(fields) => Expr::RowLiteral(
                fields
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            go(value, definitions, environment, foreign, stack, bound),
                        )
                    })
                    .collect(),
            ),
            Expr::Overloaded(cases) => Expr::Overloaded(
                cases
                    .iter()
                    .map(|case| OverloadCase {
                        annotation: case.annotation.clone(),
                        expr: Box::new(go(
                            &case.expr,
                            definitions,
                            environment,
                            foreign,
                            stack,
                            bound,
                        )),
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    go(
        expr,
        definitions,
        environment,
        foreign,
        &mut Vec::new(),
        &mut Vec::new(),
    )
}

fn annotate_lambda_parameters(expr: Expr, ty: &Type) -> Expr {
    match (expr, ty) {
        (
            Expr::Lambda {
                param,
                annotation,
                body,
            },
            Type::Function(argument, result),
        ) => Expr::Lambda {
            param,
            annotation: annotation.or_else(|| Some((**argument).clone())),
            body: Box::new(annotate_lambda_parameters(*body, result)),
        },
        (expr, _) => expr,
    }
}

fn is_sql_template_definition(expr: &Expr) -> bool {
    match expr {
        Expr::SqlTemplate(_) => true,
        Expr::Overloaded(cases) => {
            !cases.is_empty() && cases.iter().all(|case| is_sql_template_definition(&case.expr))
        }
        Expr::Annotated { expr, .. } => is_sql_template_definition(expr),
        _ => false,
    }
}

fn is_function_type(ty: &Type) -> bool {
    match ty {
        Type::Function(_, _) => true,
        Type::Overloaded(types) => !types.is_empty() && types.iter().all(is_function_type),
        _ => false,
    }
}

fn has_sql_template_function_signature(expr: &Expr, annotation: Option<&Type>) -> bool {
    match expr {
        Expr::Overloaded(cases) if !cases.is_empty() => cases.iter().all(|case| {
            is_sql_template_definition(&case.expr)
                && case.annotation.as_ref().is_some_and(is_function_type)
        }),
        _ => annotation.is_some_and(is_function_type),
    }
}

fn annotation_is_row_polymorphic(ty: &Type) -> bool {
    match ty {
        Type::RowVariable(_) => true,
        Type::RowType(_) => true,
        Type::RelationExpr(_) => true,
        Type::Relation(row) => row.columns.is_empty(),
        Type::KeyMapper
        | Type::ValueMapper
        | Type::KeyMapperOf(_, _)
        | Type::ValueMapperOf(_, _)
        | Type::KeyMapperWitness(_)
        | Type::ValueMapperWitness(_) => true,
        Type::Maybe(inner) | Type::List(inner) | Type::Aggregate(inner) | Type::Group(inner) => {
            annotation_is_row_polymorphic(inner)
        }
        Type::Function(argument, result) => {
            annotation_is_row_polymorphic(argument) || annotation_is_row_polymorphic(result)
        }
        Type::Overloaded(types) => types.iter().any(annotation_is_row_polymorphic),
        _ => false,
    }
}

fn relation_row(ty: &Type) -> Option<Row> {
    match ty {
        Type::Relation(row) => Some(row.clone()),
        Type::RelationExpr(row) => row.normalize(),
        _ => None,
    }
}
