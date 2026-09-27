/// The `(row parameter, fields)` shape of a select projection.
type ProjectionFields<'a> = (&'a str, &'a [(String, Expr)]);

/// The row literal behind a select projection: either the literal itself or
/// a single-row lambda whose body is one. The parameter names the row the
/// field expressions read from; SQL lowering maps it to the subquery alias.
fn projection_value(expr: &Expr) -> Result<ProjectionFields<'_>, String> {
    match expr {
        Expr::RowLiteral(fields) => Ok(("row", fields)),
        Expr::Lambda { param, body, .. } => match body.as_ref() {
            Expr::RowLiteral(fields) => Ok((param.as_str(), fields)),
            _ => Err("select expects a row projection".to_owned()),
        },
        Expr::Annotated { expr, .. } => projection_value(expr),
        _ => Err("select expects a row projection".to_owned()),
    }
}

/// Extract aggregate columns from an `agg` row literal: each value must be
/// an aggregate constructor application (`group .user_id`, `sum .total`,
/// `count`).
fn aggregate_projection(
    expr: &Expr,
    definitions: &HashMap<String, &Expr>,
) -> Result<Vec<AggregateField>, String> {
    let fields = match expr {
        Expr::RowLiteral(fields) => fields,
        Expr::Lambda { body, .. } => match body.as_ref() {
            Expr::RowLiteral(fields) => fields,
            _ => return Err("agg expects an aggregate projection".to_owned()),
        },
        Expr::Annotated { expr, .. } => return aggregate_projection(expr, definitions),
        _ => return Err("agg expects an aggregate projection".to_owned()),
    };
    let mut extracted = Vec::with_capacity(fields.len());
    for (alias, value) in fields {
        let (operation, field) = aggregate_key(value, definitions)?;
        if field.is_none() && !matches!(operation, AggregateOp::Count) {
            return Err(format!("aggregate '{alias}' expects a field reference"));
        }
        extracted.push(AggregateField {
            alias: alias.clone(),
            operation,
            field,
        });
    }
    Ok(extracted)
}

/// Match an aggregate constructor application. Row literals carry the user
/// spellings, so both the prelude names and `__`-primitives match; aliased
/// constructors chase through their definitions.
fn aggregate_key(
    value: &Expr,
    definitions: &HashMap<String, &Expr>,
) -> Result<(AggregateOp, Option<String>), String> {
    let (head, arguments) = flatten_apply(value);
    let head = match head {
        Expr::Annotated { expr, .. } => expr.as_ref(),
        other => other,
    };
    match head {
        Expr::Var(name) => {
            // Bindings take precedence over the fixed names, so user
            // overrides of the prelude constructors apply here too.
            if !name.starts_with("__") && definitions.contains_key(name) {
                let definition = definitions[name];
                let mut expanded = (*definition).clone();
                for argument in &arguments {
                    expanded = Expr::Apply {
                        function: Box::new(expanded),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return aggregate_key(&expanded, definitions);
            }
            let operation = match name.as_str() {
                "group" | "__group" => AggregateOp::Group,
                "count" | "__count" => AggregateOp::Count,
                "sum" | "__sum" => AggregateOp::Sum,
                "avg" | "__avg" => AggregateOp::Avg,
                "min" | "__min" => AggregateOp::Min,
                "max" | "__max" => AggregateOp::Max,
                _ => return Err(format!("unknown aggregate '{name}'")),
            };
            let field =
                arguments
                    .first()
                    .and_then(|argument| field_name_of(argument));
            Ok((operation, field))
        }
        // An aliased constructor reduces through its wrapper lambda: applied
        // arguments beta-reduce, a bare reference (`countRows = count`)
        // continues with the wrapper body.
        Expr::Lambda { param, body, .. } => {
            if let Some((first, rest)) = arguments.split_first() {
                let mut reduced = substitute(body, param, first);
                for argument in rest {
                    reduced = Expr::Apply {
                        function: Box::new(reduced),
                        argument: Box::new((*argument).clone()),
                    };
                }
                return aggregate_key(&reduced, definitions);
            }
            aggregate_key(body, definitions)
        }
        other => Err(format!(
            "agg expects an aggregate constructor, got {other:?}"
        )),
    }
}

fn field_name_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Field(field) => Some(field.clone()),
        Expr::Access { field, .. } => Some(field.clone()),
        Expr::Lambda { body, .. } => field_name_of(body),
        _ => None,
    }
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
        // Chase bindings to their primitive head; double-underscore names
        // are the primitives themselves and must not be chased again.
        Expr::Var(name) if !name.starts_with("__") && definitions.contains_key(name) => {
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
                if !name.starts_with("__") {
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

pub(super) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
