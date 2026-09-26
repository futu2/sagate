fn compile_table(name: &str, tables: &HashMap<&str, &Row>) -> Result<Relation, CompileError> {
    let row = tables
        .get(name)
        .ok_or_else(|| CompileError::new(format!("unknown table '{name}'")))?;
    let fields = row
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Relation {
        sql: format!("SELECT {fields} FROM {}", quote_ident(name)),
        row: (*row).clone(),
    })
}

fn compile_table_path(schema: &str, name: &str) -> Result<Relation, String> {
    Ok(Relation {
        sql: format!(
            "SELECT * FROM {}.{}",
            quote_ident(schema),
            quote_ident(name)
        ),
        row: Row::default(),
    })
}

fn compile_where(inner: Relation, predicate: &Predicate) -> Result<Relation, CompileError> {
    let condition = compile_predicate(predicate, &inner.row, "q")?;
    Ok(Relation {
        sql: format!("SELECT * FROM ({}) AS \"q\" WHERE {condition}", inner.sql),
        row: inner.row,
    })
}

fn compile_select(inner: Relation, fields: &[SelectField]) -> Result<Relation, CompileError> {
    let mut selections = Vec::new();
    let mut selection_positions = HashMap::new();
    let mut columns = Vec::new();
    for field in fields {
        let column = inner.row.field(&field.field);
        let selection = format!(
            "q.{} AS {}",
            quote_ident(&field.field),
            quote_ident(&field.alias)
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
        sql: format!(
            "SELECT {} FROM ({}) AS \"q\"",
            selections.join(", "),
            inner.sql
        ),
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
        let source = field
            .field
            .as_deref()
            .map(|name| format!("q.{}", quote_ident(name)));
        let expression = match &field.operation {
            AggregateOp::Named(_) => {
                return Err(CompileError::new("unresolved aggregate operation"));
            }
            AggregateOp::Group => {
                let source =
                    source.ok_or_else(|| CompileError::new("group expects a field reference"))?;
                groups.push(source.clone());
                source
            }
            AggregateOp::Count => source.map_or_else(
                || "COUNT(*)".to_owned(),
                |source| format!("COUNT({source})"),
            ),
            AggregateOp::Sum => format!(
                "SUM({})",
                source.ok_or_else(|| CompileError::new("sum expects a field reference"))?
            ),
            AggregateOp::Avg => format!(
                "AVG({})",
                source.ok_or_else(|| CompileError::new("avg expects a field reference"))?
            ),
            AggregateOp::Min => format!(
                "MIN({})",
                source.ok_or_else(|| CompileError::new("min expects a field reference"))?
            ),
            AggregateOp::Max => format!(
                "MAX({})",
                source.ok_or_else(|| CompileError::new("max expects a field reference"))?
            ),
        };
        selections.push(format!("{expression} AS {}", quote_ident(&field.alias)));
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
    let group_sql = if groups.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", groups.join(", "))
    };
    Ok(Relation {
        sql: format!(
            "SELECT {} FROM ({}) AS \"q\"{}",
            selections.join(", "),
            inner.sql,
            group_sql
        ),
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
        "\"l\".*, \"r\".*".to_owned()
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
                format!(
                    "{source}.{} AS {}",
                    quote_ident(&column.name),
                    quote_ident(&column.name)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let join_keyword = match kind {
        "__joinLeft" => "LEFT JOIN",
        "__joinRight" => "RIGHT JOIN",
        "__joinFull" => "FULL JOIN",
        _ => "INNER JOIN",
    };
    Ok(Relation {
        sql: format!(
            "SELECT {selections} FROM ({}) AS \"l\" {join_keyword} ({}) AS \"r\" ON \"l\".{} {} \"r\".{}",
            left.sql,
            right.sql,
            quote_ident(&predicate.left_field),
            compare_operator(&predicate.op)?,
            quote_ident(&predicate.right_field),
        ),
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
            sql: format!("SELECT * FROM ({}) AS \"q\"", inner.sql),
            row: inner.row,
        });
    }
    let (selections, row) = mapped_key_projection(&inner, mapper, "q");
    Ok(Relation {
        sql: format!("SELECT {selections} FROM ({}) AS \"q\"", inner.sql),
        row,
    })
}

fn compile_map_value(inner: Relation, mapper: &Mapper) -> Result<Relation, CompileError> {
    if inner.row.columns.is_empty() {
        return Ok(Relation {
            sql: format!("SELECT * FROM ({}) AS \"q\"", inner.sql),
            row: inner.row,
        });
    }
    let selections = inner
        .row
        .columns
        .iter()
        .map(|column| format!("q.{0} AS {0}", quote_ident(&column.name)))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Relation {
        sql: format!("SELECT {selections} FROM ({}) AS \"q\"", inner.sql),
        row: inner.row.map_value(mapper),
    })
}

fn compile_merge(left: Relation, right: Relation) -> Result<Relation, CompileError> {
    let row = left.row.merge(&right.row);
    if row.columns.is_empty() {
        return Ok(Relation {
            // With two catalog-backed open rows, the compiler cannot enumerate
            // collisions yet. Selecting the newer side preserves the
            // right-biased meaning for fields visible after catalog loading.
            sql: format!(
                "SELECT \"r\".* FROM ({}) AS \"l\" CROSS JOIN ({}) AS \"r\"",
                left.sql, right.sql
            ),
            row,
        });
    }
    let right_names: std::collections::HashSet<_> = right
        .row
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    let selections = row
        .columns
        .iter()
        .map(|column| {
            let source = if right_names.contains(column.name.as_str()) {
                "r"
            } else {
                "l"
            };
            format!(
                "{source}.{} AS {}",
                quote_ident(&column.name),
                quote_ident(&column.name)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Relation {
        sql: format!(
            "SELECT {selections} FROM ({}) AS \"l\" CROSS JOIN ({}) AS \"r\"",
            left.sql, right.sql
        ),
        row,
    })
}

fn mapped_key_projection(relation: &Relation, mapper: &Mapper, alias: &str) -> (String, Row) {
    let mut selections = Vec::new();
    let mut columns = Vec::new();
    let mut positions = HashMap::new();
    for column in &relation.row.columns {
        let mapped = mapper.map_key(&column.name);
        let selection = format!(
            "{alias}.{} AS {}",
            quote_ident(&column.name),
            quote_ident(&mapped)
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
    (selections.join(", "), Row::new(columns))
}

