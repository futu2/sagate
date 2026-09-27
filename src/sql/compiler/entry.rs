use std::collections::HashMap;

use crate::lang::{
    flatten_apply, substitute, type_check, AggregateField, AggregateOp, Column, Expr, Intrinsic,
    Literal, Mapper, Program, Row,
};

use super::model::{CompileError, CompiledQuery, Relation};

#[cfg(test)]
pub(crate) fn compile(program: &Program) -> Result<Vec<CompiledQuery>, String> {
    compile_with_dialect(program, "ansi")
}

pub(crate) fn compile_with_dialect(
    program: &Program,
    dialect: &str,
) -> Result<Vec<CompiledQuery>, String> {
    let dialect = sqlglot_rust::Dialect::from_str(dialect)
        .ok_or_else(|| format!("unknown SQL dialect '{dialect}'"))?;
    // Type checking validates all row labels before SQL generation.
    let known_rows = type_check(program).map_err(|error| error.to_string())?;
    let definitions: HashMap<String, &Expr> = program
        .bindings
        .iter()
        .map(|binding| (binding.name.clone(), &binding.expr))
        .collect();
    // CTE names are numbered from one shared counter, so every step in a
    // pipeline (and across pipelines) lifts its input under a fresh name.
    let mut counter = 0u32;
    let mut result = Vec::with_capacity(known_rows.len());
    for binding in &program.bindings {
        if let Some(row) = known_rows.get(&binding.name) {
            let mut relation = compile_expr(
                &binding.expr,
                &definitions,
                &HashMap::new(),
                &known_rows,
                &mut counter,
            )?;
            relation.row = row.clone();
            let statement = attach_ctes(relation.statement, relation.ctes);
            // sqlglot's passes (constant folding, boolean simplification,
            // limit-aware predicate pushdown) run over the flat CTE chain.
            let statement = sqlglot_rust::optimizer::optimize(*statement)
                .map_err(|error| error.to_string())?;
            result.push(CompiledQuery {
                name: binding.name.clone(),
                sql: render_sql(Box::new(statement), dialect),
                row: relation.row,
            });
        }
    }
    Ok(result)
}

/// Attach the accumulated CTE chain to the statement that renders it.
fn attach_ctes(mut statement: Box<Statement>, ctes: Vec<Cte>) -> Box<Statement> {
    if !ctes.is_empty() {
        if let Statement::Select(select) = statement.as_mut() {
            select.ctes = ctes;
        }
    }
    statement
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
    definitions: &HashMap<String, &Expr>,
    locals: &HashMap<String, Relation>,
    known_rows: &HashMap<String, Row>,
    counter: &mut u32,
) -> Result<Relation, String> {
    match expr {
        Expr::Apply { .. } => {
            compile_application(expr, definitions, locals, known_rows, counter)
        }
        Expr::Let { name, value, body } => {
            // Let is beta-reduced at this boundary. This supports both
            // relation bindings and local function values without introducing
            // an imperative runtime environment into SQL lowering.
            let reduced = substitute(body, name, value);
            compile_expr(&reduced, definitions, locals, known_rows, counter)
        }
        Expr::Annotated { expr, .. } => {
            compile_expr(expr, definitions, locals, known_rows, counter)
        }
        Expr::Var(name) => {
            if let Some(value) = locals.get(name) {
                return Ok(value.clone());
            }
            let value = definitions
                .get(name)
                .ok_or_else(|| format!("unknown variable '{name}'"))?;
            let mut relation = compile_expr(value, definitions, locals, known_rows, counter)?;
            if let Some(row) = known_rows.get(name) {
                relation.row = row.clone();
            }
            Ok(relation)
        }
        Expr::RowLiteral(_)
        | Expr::List(_)
        | Expr::Overloaded(_)
        | Expr::Literal(_)
        | Expr::SqlTemplate(_)
        | Expr::Field(_)
        | Expr::Access { .. }
        | Expr::Lambda { .. } => Err("expression does not produce a relation".to_owned()),
    }
}

fn compile_application(
    expr: &Expr,
    definitions: &HashMap<String, &Expr>,
    locals: &HashMap<String, Relation>,
    known_rows: &HashMap<String, Row>,
    counter: &mut u32,
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
            match compile_expr(&expanded, definitions, locals, known_rows, counter) {
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
        return compile_expr(&reduced, definitions, locals, known_rows, counter);
    }
    let Expr::Var(name) = head else {
        return Err("only prelude functions can produce SQL relations".to_owned());
    };
    match Intrinsic::from_name(name) {
        Some(Intrinsic::Table) => {
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
        Some(Intrinsic::Where) => {
            if arguments.len() != 2 {
                return Err("where expects a predicate and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let condition = where_condition(arguments[0], &inner.row, definitions)
                .map_err(|error| error.to_string())?;
            compile_where(inner, condition, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::Select) => {
            if arguments.len() != 2 {
                return Err("select expects a projection and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let (param, fields) = projection_value(arguments[0])?;
            compile_select(inner, param, fields, definitions, counter)
                .map_err(|error| error.to_string())
        }
        Some(Intrinsic::MapKey) => {
            if arguments.len() != 2 {
                return Err("mapKey expects a mapper and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let mapper = mapper_value(arguments[0], true, definitions)?;
            compile_map_key(inner, &mapper, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::MapValue) => {
            if arguments.len() != 2 {
                return Err("mapValue expects a mapper and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let mapper = mapper_value(arguments[0], false, definitions)?;
            compile_map_value(inner, &mapper, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::Aggregate) => {
            if arguments.len() != 2 {
                return Err("agg expects a projection and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let fields = aggregate_projection(arguments[0], definitions)?;
            compile_aggregate(inner, &fields, definitions, counter)
                .map_err(|error| error.to_string())
        }
        Some(intrinsic) if intrinsic.is_join() => {
            if arguments.len() != 3 {
                return Err("join expects a right query, predicate, and left query".to_owned());
            }
            let right =
                compile_expr(arguments[0], definitions, locals, known_rows, counter)?;
            let left =
                compile_expr(arguments[2], definitions, locals, known_rows, counter)?;
            let condition = join_condition(arguments[1], &left.row, &right.row, definitions)
                .map_err(|error| error.to_string())?;
            compile_join(left, right, condition, intrinsic, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::Merge) => {
            if arguments.len() != 2 {
                return Err("merge expects two relations".to_owned());
            }
            let left =
                compile_expr(arguments[0], definitions, locals, known_rows, counter)?;
            let right =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            compile_merge(left, right, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::Order) => {
            if arguments.len() != 2 {
                return Err("order expects sort keys and a relation".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            let items = order_by_items(arguments[0], &inner.row, definitions)?;
            compile_order(inner, items, counter).map_err(|error| error.to_string())
        }
        Some(Intrinsic::Limit) => {
            if arguments.len() != 2 {
                return Err("limit expects a count and a relation".to_owned());
            }
            let mut count_expr = arguments[0];
            while let Expr::Var(name) = count_expr {
                count_expr = definitions
                    .get(name)
                    .ok_or_else(|| format!("unknown variable '{name}'"))?;
            }
            let Expr::Literal(Literal::Integer(count)) = count_expr else {
                return Err("limit expects an integer literal".to_owned());
            };
            if *count < 0 {
                return Err("limit expects a non-negative integer".to_owned());
            }
            let inner =
                compile_expr(arguments[1], definitions, locals, known_rows, counter)?;
            compile_limit(inner, *count, counter).map_err(|error| error.to_string())
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
            compile_expr(&expanded, definitions, locals, known_rows, counter)
        }
    }
}
