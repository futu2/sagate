use super::*;
use crate::lang::{parse, Type};

#[test]
fn compiles_a_functional_pipeline_to_sql() {
    let program = parse(
        r#"
            users : query { id: Int, displayName: String, active: Bool } = table "public" "users"
            active = users
              & where (.active == true)
              & mapKey(snake)
              & select { id: .id, name: .display_name };
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let active = sql.iter().find(|query| query.name == "active").unwrap();
    assert!(active.sql.contains("WHERE q.\"active\" = TRUE"));
    assert!(active.sql.contains("q.\"display_name\" AS \"name\""));
}

#[test]
fn compiles_null_comparisons_with_is() {
    let program = parse(
            "users : query { name: Maybe String } = table \"public\" \"users\"\nq = users & where (.name == null);",
        )
        .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"name\" IS NULL"));
}

#[test]
fn compiles_a_bound_function_value() {
    let program = parse(
        "users : query { id: Int, displayName: String } = table \"public\" \"users\";\n\
             let snake = mapKey(snake);\
             q = users & snake & select { id: .id };",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("displayName"));
}

#[test]
fn compiles_schema_and_table_source_function() {
    let program =
        parse("q = table(\"public\", \"users\") & where (.active == true) & select { id: .id };")
            .expect("parse");
    let sql = compile(&program).expect("compile");
    assert!(sql[0].sql.contains("FROM \"public\".\"users\""));
    assert!(sql[0].sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn compiles_with_requested_sql_dialect() {
    let program =
        parse("q = table \"public\" \"users\" & where (.active == true) & select { id: .id };")
            .expect("parse");
    let queries = compile_with_dialect(&program, "mysql").expect("compile for MySQL");
    assert!(queries[0].sql.contains("q.`active` = TRUE"));
    assert!(queries[0].sql.contains("q.`id` AS `id`"));
}

#[test]
fn compiles_annotated_relation_bindings_without_query_keyword() {
    let program = parse(
        r#"
            users : query { id: int, active: bool } = table "public" "users";
            active : query { id: int } = users & where (.active == true) & select { id: .id };
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    assert_eq!(
        sql.iter()
            .map(|query| query.name.as_str())
            .collect::<Vec<_>>(),
        ["users", "active"]
    );
    assert!(sql[0].sql.contains("FROM \"public\".\"users\""));
    assert!(sql[1].sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn compiles_aggregate_and_join_pipeline_steps() {
    let program = parse(
        r#"
            users_q : query { id: int, name: string } = table "public" "users"
            orders_q : query { user_id: int, total: float } = table "public" "orders"
            totals = orders_q & agg { user_id: group .user_id, total: sum .total, rows: count }
            report = totals & inner users_q (left => right => left.user_id == right.id)
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let totals = sql.iter().find(|query| query.name == "totals").unwrap();
    assert!(totals.sql.contains("SUM(q.\"total\")"));
    assert!(totals.sql.contains("GROUP BY q.\"user_id\""));
    let report = sql.iter().find(|query| query.name == "report").unwrap();
    assert!(report.sql.contains("INNER JOIN"));
    assert!(report.sql.contains("\"l\".\"user_id\" = \"r\".\"id\""));
}

#[test]
fn reversed_join_comparison_keeps_its_meaning() {
    let program = parse(
        "lefts : query { id: int } = table \"public\" \"lefts\"\n\
             rights : query { id: int } = table \"public\" \"rights\"\n\
             q = lefts & inner rights (l => r => r.id < l.id)\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("\"l\".\"id\" > \"r\".\"id\""));
}

#[test]
fn join_left_input_overwrites_duplicate_fields() {
    let program = parse(
        "users : query { id: int, name: string } = table \"public\" \"users\"\n\
             book_info : query { user_id: int, name: string } = table \"public\" \"book_info\"\n\
             report = users & left book_info (l => r => l.id == r.user_id)\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let report = sql.iter().find(|query| query.name == "report").unwrap();
    assert!(report.sql.contains("l.\"name\" AS \"name\""));
    assert_eq!(report.row.field("name").unwrap().ty, Type::String);
}
