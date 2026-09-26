use std::collections::HashMap;

use crate::lang::{
    type_check, AggregateField, AggregateOp, Column, CompareOp, Expr, Literal, Mapper, Predicate,
    Program, Row, SelectField,
};

use super::model::{CompileError, CompiledQuery, Relation};

pub fn compile(program: &Program) -> Result<Vec<CompiledQuery>, String> {
    compile_with_dialect(program, "ansi")
}

pub fn compile_with_dialect(
    program: &Program,
    dialect: &str,
) -> Result<Vec<CompiledQuery>, String> {
    let dialect = sqlglot_rust::Dialect::from_str(dialect)
        .ok_or_else(|| format!("unknown SQL dialect '{dialect}'"))?;
    // Type checking validates all row labels before SQL generation.
    let known_rows = type_check(program).map_err(|error| error.to_string())?;
    let tables: HashMap<_, _> = program
        .tables
        .iter()
        .map(|table| (table.name.as_str(), &table.row))
        .collect();
    let definitions: HashMap<String, &Expr> = program
        .bindings
        .iter()
        .map(|binding| (binding.name.clone(), &binding.expr))
        .collect();
    let mut result = Vec::with_capacity(known_rows.len());
    for binding in &program.bindings {
        if let Some(row) = known_rows.get(&binding.name) {
            let mut relation = compile_expr(
                &binding.expr,
                &tables,
                &definitions,
                &HashMap::new(),
                &known_rows,
            )?;
            relation.row = row.clone();
            result.push(CompiledQuery {
                name: binding.name.clone(),
                sql: render_sql(relation.statement, dialect),
                row: relation.row,
            });
        }
    }
    Ok(result)
}

// The generator recursively walks nested subqueries. Keep that walk off the
// small stack used by Rust's test and async worker threads.
fn render_sql(statement: Box<sqlglot_rust::ast::Statement>, dialect: sqlglot_rust::Dialect) -> String {
    std::thread::Builder::new()
        .name("sagate-sql-render".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || sqlglot_rust::generate(&statement, dialect))
        .expect("spawn SQL renderer")
        .join()
        .expect("SQL renderer panicked")
}

fn compile_expr(
    expr: &Expr,
    tables: &HashMap<&str, &Row>,
    definitions: &HashMap<String, &Expr>,
    locals: &HashMap<String, Relation>,
    known_rows: &HashMap<String, Row>,
) -> Result<Relation, String> {
    match expr {
        Expr::Apply { .. } => compile_application(expr, tables, definitions, locals, known_rows),
        Expr::Let { name, value, body } => {
            // Let is beta-reduced at this boundary. This supports both
            // relation bindings and local function values without introducing
            // an imperative runtime environment into SQL lowering.
            let reduced = substitute(body, name, value);
            compile_expr(&reduced, tables, definitions, locals, known_rows)
        }
        Expr::Annotated { expr, .. } => compile_expr(expr, tables, definitions, locals, known_rows),
        Expr::Var(name) => {
            if let Some(value) = locals.get(name) {
                return Ok(value.clone());
            }
            let value = definitions
                .get(name)
                .ok_or_else(|| format!("unknown variable '{name}'"))?;
            let mut relation = compile_expr(value, tables, definitions, locals, known_rows)?;
            if let Some(row) = known_rows.get(name) {
                relation.row = row.clone();
            }
            Ok(relation)
        }
        Expr::Table(name) => compile_table(name, tables).map_err(|error| error.to_string()),
        Expr::Source(name) => compile_table(name, tables).map_err(|error| error.to_string()),
        Expr::Where { input, predicate } => {
            let inner = compile_expr(input, tables, definitions, locals, known_rows)?;
            compile_where(inner, predicate).map_err(|error| error.to_string())
        }
        Expr::Select { input, fields } => {
            let inner = compile_expr(input, tables, definitions, locals, known_rows)?;
            compile_select(inner, fields).map_err(|error| error.to_string())
        }
        Expr::MapKey { input, mapper } => {
            let inner = compile_expr(input, tables, definitions, locals, known_rows)?;
            compile_map_key(inner, mapper).map_err(|error| error.to_string())
        }
        Expr::MapValue { input, mapper } => {
            let inner = compile_expr(input, tables, definitions, locals, known_rows)?;
            compile_map_value(inner, mapper).map_err(|error| error.to_string())
        }
        Expr::Merge { older, newer } => {
            let left = compile_expr(older, tables, definitions, locals, known_rows)?;
            let right = compile_expr(newer, tables, definitions, locals, known_rows)?;
            compile_merge(left, right).map_err(|error| error.to_string())
        }
        Expr::Predicate(_)
        | Expr::Projection(_)
        | Expr::AggregateProjection(_)
        | Expr::Mapper { .. }
        | Expr::Overloaded(_)
        | Expr::Literal(_)
        | Expr::Field(_)
        | Expr::Access { .. }
        | Expr::Binary { .. }
        | Expr::Lambda { .. } => Err("expression does not produce a relation".to_owned()),
    }
}

fn compile_application(
    expr: &Expr,
    tables: &HashMap<&str, &Row>,
    definitions: &HashMap<String, &Expr>,
    locals: &HashMap<String, Relation>,
    known_rows: &HashMap<String, Row>,
) -> Result<Relation, String> {
    let (head, arguments) = flatten_apply(expr);
    if let Expr::Overloaded(cases) = head {
        let mut last_error = None;
        for case in cases {
            let mut expanded = (*case.expr).clone();
            for argument in &arguments {
                expanded = Expr::Apply {
                    function: Box::new(expanded),
                    argument: Box::new((*argument).clone()),
                };
            }
            match compile_expr(&expanded, tables, definitions, locals, known_rows) {
                Ok(relation) => return Ok(relation),
                Err(error) => last_error = Some(error),
            }
        }
        return Err(last_error.unwrap_or_else(|| "no overload produces a relation".to_owned()));
    }
    if let Expr::Lambda { param, body, .. } = head {
        if arguments.is_empty() {
            return Err("a lambda needs an argument before it can produce SQL".to_owned());
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
        return compile_expr(&reduced, tables, definitions, locals, known_rows);
    }
    let Expr::Var(name) = head else {
        return Err("only prelude functions can produce SQL relations".to_owned());
    };
    match name.as_str() {
        "__table" => {
            if arguments.len() != 2 {
                return Err("table expects a schema name and table name".to_owned());
            }
            let Expr::Literal(Literal::String(schema)) = arguments[0] else {
                return Err("table expects a string schema name".to_owned());
            };
            let Expr::Literal(Literal::String(table)) = arguments[1] else {
                return Err("table expects a string table name".to_owned());
            };
            compile_table_path(schema, table)
        }
        "__where" => {
            if arguments.len() != 2 {
                return Err("where expects a predicate and a relation".to_owned());
            }
            let inner = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            let predicate = predicate_value(arguments[0], definitions)?;
            compile_where(inner, &predicate).map_err(|error| error.to_string())
        }
        "__select" => {
            if arguments.len() != 2 {
                return Err("select expects a projection and a relation".to_owned());
            }
            let inner = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            let fields = projection_value(arguments[0])?;
            compile_select(inner, fields).map_err(|error| error.to_string())
        }
        "__mapKey" => {
            if arguments.len() != 2 {
                return Err("mapKey expects a mapper and a relation".to_owned());
            }
            let inner = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            let mapper = mapper_value(arguments[0], true, definitions)?;
            compile_map_key(inner, &mapper).map_err(|error| error.to_string())
        }
        "__mapValue" => {
            if arguments.len() != 2 {
                return Err("mapValue expects a mapper and a relation".to_owned());
            }
            let inner = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            let mapper = mapper_value(arguments[0], false, definitions)?;
            compile_map_value(inner, &mapper).map_err(|error| error.to_string())
        }
        "__agg" => {
            if arguments.len() != 2 {
                return Err("agg expects a projection and a relation".to_owned());
            }
            let inner = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            let fields = aggregate_projection(arguments[0])?;
            compile_aggregate(inner, fields).map_err(|error| error.to_string())
        }
        "__joinInner" | "__joinLeft" | "__joinRight" | "__joinFull" => {
            if arguments.len() != 3 {
                return Err("join expects a right query, predicate, and left query".to_owned());
            }
            let right = compile_expr(arguments[0], tables, definitions, locals, known_rows)?;
            let predicate = join_predicate_value(arguments[1], definitions)?;
            let left = compile_expr(arguments[2], tables, definitions, locals, known_rows)?;
            compile_join(left, right, &predicate, name).map_err(|error| error.to_string())
        }
        "__merge" => {
            if arguments.len() != 2 {
                return Err("merge expects two relations".to_owned());
            }
            let left = compile_expr(arguments[0], tables, definitions, locals, known_rows)?;
            let right = compile_expr(arguments[1], tables, definitions, locals, known_rows)?;
            compile_merge(left, right).map_err(|error| error.to_string())
        }
        _ => {
            let definition = definitions
                .get(name)
                .ok_or_else(|| format!("unknown function '{name}'"))?;
            // A binding can hold a partially applied prelude function. Inline
            // it at the call site, then the normal prelude branch handles the
            // completed application.
            let mut expanded = (*definition).clone();
            for argument in arguments {
                expanded = Expr::Apply {
                    function: Box::new(expanded),
                    argument: Box::new(argument.clone()),
                };
            }
            compile_expr(&expanded, tables, definitions, locals, known_rows)
        }
    }
}

fn substitute(expr: &Expr, name: &str, replacement: &Expr) -> Expr {
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
        Expr::Binary { op, left, right } => Expr::Binary {
            op: op.clone(),
            left: Box::new(substitute(left, name, replacement)),
            right: Box::new(substitute(right, name, replacement)),
        },
        Expr::Overloaded(cases) => Expr::Overloaded(
            cases
                .iter()
                .map(|case| crate::lang::OverloadCase {
                    annotation: case.annotation.clone(),
                    expr: Box::new(substitute(&case.expr, name, replacement)),
                })
                .collect(),
        ),
        other => other.clone(),
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
