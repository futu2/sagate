use sqlglot_rust::ast::{
    Cte, FromClause, JoinClause, JoinType, SelectItem, SelectStatement, Statement, TableRef,
    TableSource,
};


fn compile_table_path(schema: &str, name: &str) -> Result<Relation, String> {
    Ok(Relation {
        ctes: vec![],
        statement: Box::new(select_statement(
            vec![SelectItem::Wildcard],
            table_source(name, Some(schema)),
            vec![],
            None,
            vec![],
        )),
        row: Row::default(),
    })
}

fn fresh_cte(prefix: &str, counter: &mut u32) -> String {
    let name = format!("{prefix}{counter}");
    *counter += 1;
    name
}

fn cte(name: &str, query: Box<Statement>) -> Cte {
    Cte {
        name: name.to_owned(),
        name_quote_style: QuoteStyle::DoubleQuote,
        columns: vec![],
        query,
        materialized: None,
        recursive: false,
    }
}

/// A pipeline step lifts its input relation into a CTE and reads it back
/// through a plain table source under the step's row scope alias, so a
/// pipeline compiles to one flat `WITH` chain instead of nested subqueries.
fn cte_source(name: &str, alias: &str) -> TableSource {
    TableSource::Table(TableRef {
        catalog: None,
        schema: None,
        name: name.to_owned(),
        alias: Some(alias.to_owned()),
        temporal: None,
        name_quote_style: QuoteStyle::DoubleQuote,
        alias_quote_style: QuoteStyle::None,
    })
}

fn compile_where(
    inner: Relation,
    condition: SqlExpr,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row,
    } = inner;
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            vec![SelectItem::Wildcard],
            cte_source(&name, "q"),
            vec![],
            Some(condition),
            vec![],
        )),
        row,
    })
}

fn compile_order(
    inner: Relation,
    items: Vec<OrderByItem>,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row,
    } = inner;
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    let mut outer = Box::new(select_statement(
        vec![SelectItem::Wildcard],
        cte_source(&name, "q"),
        vec![],
        None,
        vec![],
    ));
    if let Statement::Select(select) = outer.as_mut() {
        select.order_by = items;
    }
    Ok(Relation {
        ctes,
        statement: outer,
        row,
    })
}

fn compile_limit(inner: Relation, count: i64, counter: &mut u32) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row,
    } = inner;
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    let mut outer = Box::new(select_statement(
        vec![SelectItem::Wildcard],
        cte_source(&name, "q"),
        vec![],
        None,
        vec![],
    ));
    if let Statement::Select(select) = outer.as_mut() {
        select.limit = Some(SqlExpr::Number(count.to_string()));
    }
    Ok(Relation {
        ctes,
        statement: outer,
        row,
    })
}

fn compile_select(
    inner: Relation,
    param: &str,
    fields: &[(String, Expr)],
    definitions: &HashMap<String, &Expr>,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row: inner_row,
    } = inner;
    let scope = RowScope {
        params: vec![(param.to_owned(), "q", &inner_row)],
    };
    let mut selections = Vec::new();
    let mut selection_positions = HashMap::new();
    let mut columns = Vec::new();
    for (name, value) in fields {
        let sql =
            lower_row_expression(value, &scope, definitions).map_err(CompileError::new)?;
        let selection = select_expr(sql, Some(name.clone()));
        if let Some(index) = selection_positions.get(name).copied() {
            selections[index] = selection;
        } else {
            selection_positions.insert(name.clone(), selections.len());
            selections.push(selection);
        }
        columns.push(Column {
            name: name.clone(),
            ty: projection_field_type(value, &inner_row),
        });
    }
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&name, "q"),
            vec![],
            None,
            vec![],
        )),
        row: Row::new(columns),
    })
}

/// The static type of a projection value. Field references keep their column
/// type; computed values are refined by the checker's known rows for the
/// final binding, so `Any` is enough for mid-chain field-existence checks.
fn projection_field_type(value: &Expr, row: &Row) -> crate::lang::Type {
    match value {
        Expr::Field(field) | Expr::Access { field, .. } => row
            .field(field)
            .map_or(crate::lang::Type::Any, |column| column.ty.clone()),
        Expr::Lambda { body, .. } => projection_field_type(body, row),
        _ => crate::lang::Type::Any,
    }
}

fn compile_aggregate(
    inner: Relation,
    fields: &[AggregateField],
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    if fields.is_empty() {
        return Err(CompileError::new("agg expects at least one field"));
    }
    let Relation {
        mut ctes,
        statement,
        row: inner_row,
    } = inner;
    let mut selections = Vec::with_capacity(fields.len());
    let mut groups = Vec::new();
    let mut columns = Vec::with_capacity(fields.len());
    for field in fields {
        let source = field.field.as_deref().map(|name| sql_column(name, Some("q")));
        let expression = match &field.operation {
            AggregateOp::Named(_) => {
                return Err(CompileError::new("unresolved aggregate operation"));
            }
            AggregateOp::Group => {
                let source = source
                    .ok_or_else(|| CompileError::new("group expects a field reference"))?;
                groups.push(source.clone());
                source
            }
            AggregateOp::Count => aggregate_call("COUNT", source.unwrap_or(SqlExpr::Wildcard)),
            AggregateOp::Sum => aggregate_call(
                "SUM",
                source.ok_or_else(|| CompileError::new("sum expects a field reference"))?,
            ),
            AggregateOp::Avg => aggregate_call(
                "AVG",
                source.ok_or_else(|| CompileError::new("avg expects a field reference"))?,
            ),
            AggregateOp::Min => aggregate_call(
                "MIN",
                source.ok_or_else(|| CompileError::new("min expects a field reference"))?,
            ),
            AggregateOp::Max => aggregate_call(
                "MAX",
                source.ok_or_else(|| CompileError::new("max expects a field reference"))?,
            ),
        };
        selections.push(select_expr(expression, Some(field.alias.clone())));
        let ty = match &field.operation {
            AggregateOp::Named(_) => {
                return Err(CompileError::new("unresolved aggregate operation"));
            }
            AggregateOp::Count => crate::lang::Type::Int,
            AggregateOp::Avg => crate::lang::Type::Float,
            AggregateOp::Group | AggregateOp::Sum | AggregateOp::Min | AggregateOp::Max => field
                .field
                .as_deref()
                .and_then(|name| inner_row.field(name))
                .map_or(crate::lang::Type::Any, |column| column.ty.clone()),
        };
        columns.push(Column {
            name: field.alias.clone(),
            ty,
        });
    }
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&name, "q"),
            vec![],
            None,
            groups,
        )),
        row: Row::new(columns),
    })
}

fn compile_join(
    left: Relation,
    right: Relation,
    condition: SqlExpr,
    kind: &str,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let row = joined_row(&left.row, &right.row, kind);
    let selections = if row.columns.is_empty() {
        vec![
            SelectItem::QualifiedWildcard {
                table: "l".to_owned(),
            },
            SelectItem::QualifiedWildcard {
                table: "r".to_owned(),
            },
        ]
    } else {
        let left_names: std::collections::HashSet<_> = left
            .row
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        row.columns
            .iter()
            .map(|column| {
                let source = if left_names.contains(column.name.as_str()) {
                    "l"
                } else {
                    "r"
                };
                select_expr(
                    sql_projection_column(&column.name, source),
                    Some(column.name.clone()),
                )
            })
            .collect()
    };
    let join_type = match kind {
        "__joinLeft" => JoinType::Left,
        "__joinRight" => JoinType::Right,
        "__joinFull" => JoinType::Full,
        _ => JoinType::Inner,
    };
    let Relation {
        mut ctes,
        statement: left_statement,
        ..
    } = left;
    let Relation {
        ctes: right_ctes,
        statement: right_statement,
        ..
    } = right;
    ctes.extend(right_ctes);
    let l_name = fresh_cte("l", counter);
    let r_name = fresh_cte("r", counter);
    ctes.push(cte(&l_name, left_statement));
    ctes.push(cte(&r_name, right_statement));
    let join = JoinClause {
        join_type,
        table: cte_source(&r_name, "r"),
        on: Some(condition),
        using: vec![],
    };
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&l_name, "l"),
            vec![join],
            None,
            vec![],
        )),
        row,
    })
}

fn joined_row(left: &Row, right: &Row, kind: &str) -> Row {
    let mut row = right.merge(left);
    let nullable_left = matches!(kind, "__joinRight" | "__joinFull");
    let nullable_right = matches!(kind, "__joinLeft" | "__joinFull");
    for column in &mut row.columns {
        let from_left = left.field(&column.name).is_some();
        if (from_left && nullable_left) || (!from_left && nullable_right) {
            column.ty = nullable(column.ty.clone());
        }
    }
    row
}

fn nullable(ty: crate::lang::Type) -> crate::lang::Type {
    if matches!(ty, crate::lang::Type::Maybe(_)) {
        ty
    } else {
        crate::lang::Type::Maybe(Box::new(ty))
    }
}

fn compile_map_key(
    inner: Relation,
    mapper: &Mapper,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row: inner_row,
    } = inner;
    if inner_row.columns.is_empty() {
        let name = fresh_cte("q", counter);
        ctes.push(cte(&name, statement));
        return Ok(Relation {
            ctes,
            statement: Box::new(select_statement(
                vec![SelectItem::Wildcard],
                cte_source(&name, "q"),
                vec![],
                None,
                vec![],
            )),
            row: inner_row,
        });
    }
    let (selections, row) = mapped_key_projection(&inner_row, mapper, "q");
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&name, "q"),
            vec![],
            None,
            vec![],
        )),
        row,
    })
}

fn compile_map_value(
    inner: Relation,
    mapper: &Mapper,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let Relation {
        mut ctes,
        statement,
        row: inner_row,
    } = inner;
    let selections = inner_row
        .columns
        .iter()
        .map(|column| {
            select_expr(
                sql_column(&column.name, Some("q")),
                Some(column.name.clone()),
            )
        })
        .collect();
    let name = fresh_cte("q", counter);
    ctes.push(cte(&name, statement));
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&name, "q"),
            vec![],
            None,
            vec![],
        )),
        row: inner_row.map_value(mapper),
    })
}

fn compile_merge(
    left: Relation,
    right: Relation,
    counter: &mut u32,
) -> Result<Relation, CompileError> {
    let row = left.row.merge(&right.row);
    let selections = if row.columns.is_empty() {
        vec![
            SelectItem::QualifiedWildcard {
                table: "r".to_owned(),
            },
        ]
    } else {
        let right_names: std::collections::HashSet<_> = right
            .row
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        row.columns
            .iter()
            .map(|column| {
                let source = if right_names.contains(column.name.as_str()) {
                    "r"
                } else {
                    "l"
                };
                select_expr(
                    sql_projection_column(&column.name, source),
                    Some(column.name.clone()),
                )
            })
            .collect()
    };
    let Relation {
        mut ctes,
        statement: left_statement,
        ..
    } = left;
    let Relation {
        ctes: right_ctes,
        statement: right_statement,
        ..
    } = right;
    ctes.extend(right_ctes);
    let l_name = fresh_cte("l", counter);
    let r_name = fresh_cte("r", counter);
    ctes.push(cte(&l_name, left_statement));
    ctes.push(cte(&r_name, right_statement));
    let join = JoinClause {
        join_type: JoinType::Cross,
        table: cte_source(&r_name, "r"),
        on: None,
        using: vec![],
    };
    Ok(Relation {
        ctes,
        statement: Box::new(select_statement(
            selections,
            cte_source(&l_name, "l"),
            vec![join],
            None,
            vec![],
        )),
        row,
    })
}

fn mapped_key_projection(row: &Row, mapper: &Mapper, alias: &str) -> (Vec<SelectItem>, Row) {
    let mut selections = Vec::new();
    let mut columns = Vec::new();
    let mut positions = HashMap::new();
    for column in &row.columns {
        let mapped = mapper.map_key(&column.name);
        let selection = select_expr(
            sql_column(&column.name, Some(alias)),
            Some(mapped.clone()),
        );
        if let Some(index) = positions.get(&mapped).copied() {
            selections[index] = selection;
            columns[index] = Column {
                name: mapped,
                ty: column.ty.clone(),
            };
        } else {
            positions.insert(mapped.clone(), columns.len());
            selections.push(selection);
            columns.push(Column {
                name: mapped,
                ty: column.ty.clone(),
            });
        }
    }
    (selections, Row::new(columns))
}

fn select_statement(
    columns: Vec<SelectItem>,
    source: TableSource,
    joins: Vec<JoinClause>,
    where_clause: Option<SqlExpr>,
    group_by: Vec<SqlExpr>,
) -> Statement {
    Statement::Select(SelectStatement {
        comments: vec![],
        ctes: vec![],
        distinct: false,
        top: None,
        columns,
        from: Some(FromClause { source }),
        joins,
        where_clause,
        group_by,
        having: None,
        order_by: vec![],
        limit: None,
        offset: None,
        fetch_first: None,
        qualify: None,
        window_definitions: vec![],
        query_options: None,
    })
}

fn table_source(name: &str, schema: Option<&str>) -> TableSource {
    TableSource::Table(TableRef {
        catalog: None,
        // `sqlglot-rust` currently emits schema names verbatim (the AST has
        // no schema quote-style field), so preserve the compiler's existing
        // ANSI quoting here.
        schema: schema.map(quote_ident),
        name: name.to_owned(),
        alias: None,
        temporal: None,
        name_quote_style: QuoteStyle::DoubleQuote,
        alias_quote_style: QuoteStyle::None,
    })
}

fn sql_column(name: &str, table: Option<&str>) -> SqlExpr {
    SqlExpr::Column {
        table: table.map(str::to_owned),
        name: name.to_owned(),
        quote_style: QuoteStyle::DoubleQuote,
        table_quote_style: if matches!(table, Some("q")) {
            QuoteStyle::None
        } else {
            QuoteStyle::DoubleQuote
        },
    }
}

fn sql_projection_column(name: &str, table: &str) -> SqlExpr {
    SqlExpr::Column {
        table: Some(table.to_owned()),
        name: name.to_owned(),
        quote_style: QuoteStyle::DoubleQuote,
        table_quote_style: QuoteStyle::None,
    }
}

fn select_expr(expr: SqlExpr, alias: Option<String>) -> SelectItem {
    SelectItem::Expr {
        expr,
        alias,
        alias_quote_style: QuoteStyle::DoubleQuote,
    }
}

fn aggregate_call(name: &str, argument: SqlExpr) -> SqlExpr {
    SqlExpr::Function {
        name: name.to_owned(),
        args: vec![argument],
        distinct: false,
        filter: None,
        over: None,
        order_by: vec![],
        within_group: false,
    }
}
