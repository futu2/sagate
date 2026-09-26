fn primitive_head(
    expr: &Expr,
    definitions: &HashMap<String, Expr>,
    seen: &mut Vec<String>,
) -> Option<String> {
    match expr {
        Expr::Var(name) => {
            if name.starts_with("__") {
                return Some(name.clone());
            }
            if seen.contains(name) {
                return None;
            }
            let definition = definitions.get(name)?;
            seen.push(name.clone());
            let result = primitive_head(definition, definitions, seen);
            seen.pop();
            result
        }
        Expr::Lambda { body, .. } => primitive_head(body, definitions, seen),
        Expr::Apply { function, .. } => primitive_head(function, definitions, seen),
        Expr::Annotated { expr, .. } => primitive_head(expr, definitions, seen),
        Expr::Overloaded(cases) => {
            let mut primitive = None;
            for case in cases {
                let current = primitive_head(&case.expr, definitions, seen)?;
                if primitive.as_deref().is_some_and(|known| known != current) {
                    return None;
                }
                primitive = Some(current);
            }
            primitive
        }
        _ => None,
    }
}

fn resolve_prelude_operations(
    expr: &mut Expr,
    definitions: &HashMap<String, Expr>,
) -> Result<(), String> {
    match expr {
        Expr::Var(name) if is_infix_operator(name) => {
            let primitive = primitive_head(&Expr::Var(name.clone()), definitions, &mut Vec::new());
            if primitive.as_deref().and_then(comparison_operator).is_some() {
                *name = primitive.unwrap();
            }
        }
        Expr::Predicate(predicate) => {
            if let CompareOp::Named(name) = &predicate.op {
                let primitive =
                    primitive_head(&Expr::Var(name.clone()), definitions, &mut Vec::new());
                predicate.op = primitive
                    .as_deref()
                    .and_then(comparison_operator)
                    .ok_or_else(|| format!("unknown comparison operator '{name}'"))?;
            }
        }
        Expr::AggregateProjection(fields) => {
            for field in fields {
                let AggregateOp::Named(name) = &field.operation else {
                    continue;
                };
                let primitive =
                    primitive_head(&Expr::Var(name.clone()), definitions, &mut Vec::new())
                        .ok_or_else(|| format!("unknown aggregate '{name}'"))?;
                let operation = match primitive.as_str() {
                    "__group" => AggregateOp::Group,
                    "__count" => AggregateOp::Count,
                    "__sum" => AggregateOp::Sum,
                    "__avg" => AggregateOp::Avg,
                    "__min" => AggregateOp::Min,
                    "__max" => AggregateOp::Max,
                    _ => return Err(format!("unknown aggregate '{name}'")),
                };
                if field.field.is_none() && !matches!(operation, AggregateOp::Count) {
                    return Err("aggregate expects a field reference".to_owned());
                }
                field.operation = operation;
            }
        }
        Expr::Apply { function, argument }
        | Expr::Binary {
            left: function,
            right: argument,
            ..
        } => {
            resolve_prelude_operations(function, definitions)?;
            resolve_prelude_operations(argument, definitions)?;
        }
        Expr::Lambda { body, .. }
        | Expr::Annotated { expr: body, .. }
        | Expr::Access { target: body, .. }
        | Expr::Where { input: body, .. }
        | Expr::Select { input: body, .. }
        | Expr::MapKey { input: body, .. }
        | Expr::MapValue { input: body, .. } => resolve_prelude_operations(body, definitions)?,
        Expr::Let { value, body, .. }
        | Expr::Merge {
            older: value,
            newer: body,
        } => {
            resolve_prelude_operations(value, definitions)?;
            resolve_prelude_operations(body, definitions)?;
        }
        Expr::Overloaded(cases) => {
            for case in cases {
                resolve_prelude_operations(&mut case.expr, definitions)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn parse(source: &str) -> Result<Program, String> {
    let mut program = Parser::new(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prelude.sagate")))?.parse_program()?;
    let user = Parser::new(source)?.parse_program()?;
    program.tables.extend(user.tables);
    for mut binding in user.bindings {
        if let Some(index) = program
            .bindings
            .iter()
            .position(|existing| existing.name == binding.name)
        {
            // Preserve the original value for references inside an override.
            let original_name = binding.name.clone();
            let hidden_name = format!("$prelude_{index}");
            program.bindings[index].name = hidden_name.clone();
            binding.expr = substitute(&binding.expr, &original_name, &Expr::Var(hidden_name));
        }
        program.bindings.push(binding);
    }
    let definitions: HashMap<_, _> = program
        .bindings
        .iter()
        .map(|binding| (binding.name.clone(), binding.expr.clone()))
        .collect();
    for binding in &mut program.bindings {
        resolve_prelude_operations(&mut binding.expr, &definitions)?;
    }
    Ok(program)
}

