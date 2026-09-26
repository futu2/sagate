use sqlglot_rust::ast::{
    FromClause, JoinClause, JoinType, SelectItem, SelectStatement, Statement, TableRef,
    TableSource,
};

fn compile_table(name: &str, tables: &HashMap<&str, &Row>) -> Result<Relation, CompileError> {
    let row = tables
        .get(name)
        .ok_or_else(|| CompileError::new(format!("unknown table '{name}'")))?;
    let fields = row
        .columns
        .iter()
        .map(|column| select_expr(sql_column(&column.name, None), None))
        .collect();
    Ok(Relation {
        statement: Box::new(select_statement(fields, table_source(name, None), vec![], None, vec![])),
        row: (*row).clone(),
    })
}

fn compile_table_path(schema: &str, name: &str) -> Result<Relation, String> {
    Ok(Relation {
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

fn compile_where(inner: Relation, predicate: &Predicate) -> Result<Relation, CompileError> {
    let condition = compile_predicate(predicate, &inner.row, "q")?;
    Ok(Relation {
        statement: Box::new(select_statement(
            vec![SelectItem::Wildcard],
            subquery_source(inner.statement.clone(), "q"),
            vec![],
            Some(condition),
            vec![],
        )),
        row: inner.row,
    })
}

fn compile_select(inner: Relation, fields: &[SelectField]) -> Result<Relation, CompileError> {
    let mut selections = Vec::new();
    let mut selection_positions = HashMap::new();
    let mut columns = Vec::new();
    for field in fields {
        let column = inner.row.field(&field.field);
        let selection = select_expr(
            sql_column(&field.field, Some("q")),
            Some(field.alias.clone()),
        );
        if let Some(index) = selection_positions.get(&field.alias).copied() {
            selections[index] = selection;
        } else {
            selection_positions.insert(field.alias.clone(), selections.len());
            selections.push(selection);
        }
        columns.push(Column {
            name: field.alias.clone(),
            ty: column.map_or(crate::lang::Type::Any, |column| column.ty.clone()),
        });
    }
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(inner.statement, "q"),
            vec![],
            None,
            vec![],
        )),
        row: Row::new(columns),
    })
}

fn compile_aggregate(inner: Relation, fields: &[AggregateField]) -> Result<Relation, CompileError> {
    if fields.is_empty() {
        return Err(CompileError::new("agg expects at least one field"));
    }
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
                .and_then(|name| inner.row.field(name))
                .map_or(crate::lang::Type::Any, |column| column.ty.clone()),
        };
        columns.push(Column {
            name: field.alias.clone(),
            ty,
        });
    }
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(inner.statement, "q"),
            vec![],
            None,
            groups,
        )),
        row: Row::new(columns),
    })
}

#[derive(Clone)]
struct JoinPredicate {
    left_field: String,
    right_field: String,
    op: CompareOp,
}

fn compile_join(
    left: Relation,
    right: Relation,
    predicate: &JoinPredicate,
    kind: &str,
) -> Result<Relation, CompileError> {
    if left.row.field(&predicate.left_field).is_none() && !left.row.columns.is_empty() {
        return Err(CompileError::new(format!(
            "unknown field '{}' in left join input",
            predicate.left_field
        )));
    }
    if right.row.field(&predicate.right_field).is_none() && !right.row.columns.is_empty() {
        return Err(CompileError::new(format!(
            "unknown field '{}' in right join input",
            predicate.right_field
        )));
    }
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
    let condition = SqlExpr::BinaryOp {
        left: Box::new(sql_column(&predicate.left_field, Some("l"))),
        op: compare_operator(&predicate.op)?,
        right: Box::new(sql_column(&predicate.right_field, Some("r"))),
    };
    let join = JoinClause {
        join_type,
        table: subquery_source(right.statement, "r"),
        on: Some(condition),
        using: vec![],
    };
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(left.statement, "l"),
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

fn compile_map_key(inner: Relation, mapper: &Mapper) -> Result<Relation, CompileError> {
    if inner.row.columns.is_empty() {
        return Ok(Relation {
            statement: Box::new(select_statement(
                vec![SelectItem::Wildcard],
                subquery_source(inner.statement, "q"),
                vec![],
                None,
                vec![],
            )),
            row: inner.row,
        });
    }
    let (selections, row) = mapped_key_projection(&inner, mapper, "q");
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(inner.statement, "q"),
            vec![],
            None,
            vec![],
        )),
        row,
    })
}

fn compile_map_value(inner: Relation, mapper: &Mapper) -> Result<Relation, CompileError> {
    if inner.row.columns.is_empty() {
        return Ok(Relation {
            statement: Box::new(select_statement(
                vec![SelectItem::Wildcard],
                subquery_source(inner.statement, "q"),
                vec![],
                None,
                vec![],
            )),
            row: inner.row,
        });
    }
    let selections = inner
        .row
        .columns
        .iter()
        .map(|column| {
            select_expr(
                sql_column(&column.name, Some("q")),
                Some(column.name.clone()),
            )
        })
        .collect();
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(inner.statement, "q"),
            vec![],
            None,
            vec![],
        )),
        row: inner.row.map_value(mapper),
    })
}

fn compile_merge(left: Relation, right: Relation) -> Result<Relation, CompileError> {
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
    let join = JoinClause {
        join_type: JoinType::Cross,
        table: subquery_source(right.statement, "r"),
        on: None,
        using: vec![],
    };
    Ok(Relation {
        statement: Box::new(select_statement(
            selections,
            subquery_source(left.statement, "l"),
            vec![join],
            None,
            vec![],
        )),
        row,
    })
}

fn mapped_key_projection(relation: &Relation, mapper: &Mapper, alias: &str) -> (Vec<SelectItem>, Row) {
    let mut selections = Vec::new();
    let mut columns = Vec::new();
    let mut positions = HashMap::new();
    for column in &relation.row.columns {
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
        schema: schema.map(|schema| quote_ident(schema)),
        name: name.to_owned(),
        alias: None,
        temporal: None,
        name_quote_style: QuoteStyle::DoubleQuote,
        alias_quote_style: QuoteStyle::None,
    })
}

fn subquery_source(statement: Box<Statement>, alias: &str) -> TableSource {
    TableSource::Subquery {
        query: statement,
        alias: Some(alias.to_owned()),
        alias_quote_style: QuoteStyle::None,
    }
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
