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
        // Primitives must be declared by the prelude; user code may reference
        // a declared primitive (to build a custom combinator) but never an
        // unknown one.
        Expr::Var(name) if name.starts_with("__") && !definitions.contains_key(name) => {
            return Err(format!("unknown primitive '{name}'"));
        }
        Expr::Var(name) if is_infix_operator(name) => {
            let primitive = primitive_head(&Expr::Var(name.clone()), definitions, &mut Vec::new());
            if primitive.as_deref().is_some_and(is_scalar_primitive) {
                *name = primitive.unwrap();
            }
        }
        Expr::RowLiteral(fields) => {
            for (_, value) in fields {
                resolve_prelude_operations(value, definitions)?;
            }
        }
        Expr::Apply { function, argument } => {
            resolve_prelude_operations(function, definitions)?;
            resolve_prelude_operations(argument, definitions)?;
        }
        Expr::Lambda { body, .. }
        | Expr::Annotated { expr: body, .. }
        | Expr::Access { target: body, .. } => resolve_prelude_operations(body, definitions)?,
        Expr::Let { value, body, .. } => {
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
    let mut program =
        Parser::new(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prelude.sagate")))?
            .allowing_primitives()
            .parse_program()?;
    let user = Parser::new(source)?.parse_program()?;
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

