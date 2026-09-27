use super::*;
use crate::lang::{parse, type_check, Type};

#[test]
fn compiles_a_functional_pipeline_to_sql() {
    let program = parse(
        r#"
            users : query { id = Int, displayName = String, active = Bool } = table "public" "users"
            active = users
              & where (.active == true)
              & mapKey(snake)
              & select { id = .id, name = .display_name };
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let active = sql.iter().find(|query| query.name == "active").unwrap();
    assert!(active.sql.contains("WITH \"q0\" AS (SELECT * FROM"));
    assert!(active.sql.contains("WHERE q.\"active\" = TRUE"));
    assert!(active.sql.contains("q.\"display_name\" AS \"name\""));
}

#[test]
fn compiles_null_comparisons_with_is() {
    let program = parse(
            "users : query { name = Maybe String } = table \"public\" \"users\"\nq = users & where (.name == null);",
        )
        .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"name\" IS NULL"));
}

#[test]
fn predicates_lift_general_expressions_to_sql() {
    let program = parse(
        "orders : query { price = float, qty = int, discount = float, created_at = timestamp } = table \"public\" \"orders\"\n\
             big = orders & where (.price * .qty > 100)\n\
             recent = orders & where (row => row.created_at >= timestamp \"2026-01-01T00:00:00Z\" && row.price + 1.5 < 10.0)\n\
             uneven = orders & where (.price != .discount)\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");
    let sql = compile(&program).expect("compile");
    let find = |name: &str| sql.iter().find(|query| query.name == name).unwrap();
    assert!(find("big").sql.contains("q.\"price\" * q.\"qty\" > 100"));
    let recent = find("recent").sql.clone();
    assert!(recent.contains(">= '2026-01-01T00:00:00Z'"));
    assert!(recent.contains("q.\"price\" + 1.5 < 10.0"));
    assert!(recent.contains(" AND "));
    assert!(find("uneven").sql.contains("q.\"price\" <> q.\"discount\""));
}

#[test]
fn compiles_a_bound_function_value() {
    let program = parse(
        "users : query { id = Int, displayName = String } = table \"public\" \"users\";\n\
             let snake = mapKey(snake);\
             q = users & snake & select { id = .id };",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("displayName"));
}

#[test]
fn compiles_schema_and_table_source_function() {
    let program =
        parse("q = table(\"public\", \"users\") & where (.active == true) & select { id = .id };")
            .expect("parse");
    let sql = compile(&program).expect("compile");
    assert!(sql[0].sql.contains("FROM \"public\".\"users\""));
    assert!(sql[0].sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn compiles_with_requested_sql_dialect() {
    let program =
        parse("q = table \"public\" \"users\" & where (.active == true) & select { id = .id };")
            .expect("parse");
    let queries = compile_with_dialect(&program, "mysql").expect("compile for MySQL");
    assert!(queries[0].sql.contains("q.`active` = TRUE"));
    assert!(queries[0].sql.contains("q.`id` AS `id`"));
}

#[test]
fn compiles_annotated_relation_bindings_without_query_keyword() {
    let program = parse(
        r#"
            users : query { id = int, active = bool } = table "public" "users";
            active : query { id = int } = users & where (.active == true) & select { id = .id };
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
            users_q : query { id = int, name = string } = table "public" "users"
            orders_q : query { user_id = int, total = float } = table "public" "orders"
            totals = orders_q & agg { user_id = group .user_id, total = sum .total, rows = count }
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
        "lefts : query { id = int } = table \"public\" \"lefts\"\n\
             rights : query { id = int } = table \"public\" \"rights\"\n\
             q = lefts & inner rights (l => r => r.id < l.id)\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("\"r\".\"id\" < \"l\".\"id\""));
}

#[test]
fn join_left_input_overwrites_duplicate_fields() {
    let program = parse(
        "users : query { id = int, name = string } = table \"public\" \"users\"\n\
             book_info : query { user_id = int, name = string } = table \"public\" \"book_info\"\n\
             report = users & left book_info (l => r => l.id == r.user_id)\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let report = sql.iter().find(|query| query.name == "report").unwrap();
    assert!(report.sql.contains("l.\"name\" AS \"name\""));
    assert_eq!(report.row.field("name").unwrap().ty, Type::String);
}

#[test]
fn compiles_order_and_limit_pipeline_steps() {
    let program = parse(
        r#"
            users : query { id = int, lastName = string, age = int } = table "public" "users"
            oldest = users & order [desc .age] & limit 10
            by_name = users & order [asc .lastName, desc .age]
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let find = |name: &str| sql.iter().find(|query| query.name == name).unwrap();
    let oldest = find("oldest").sql.clone();
    assert!(oldest.contains("ORDER BY q.\"age\" DESC"));
    assert!(oldest.contains("LIMIT 10"));
    let by_name = find("by_name").sql.clone();
    assert!(by_name.contains("ORDER BY q.\"lastName\", q.\"age\" DESC"));
}

#[test]
fn compiles_bare_order_keys_as_ascending() {
    let program = parse(
        "users : query { id = int, name = string } = table \"public\" \"users\"\n\
             q = users & order [asc .name]\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("ORDER BY q.\"name\""));
    assert!(!q.sql.contains("DESC"));
}

#[test]
fn compiles_order_keys_as_a_list_literal() {
    let program = parse(
        "users : query { id = int, created_at = timestamp } = table \"public\" \"users\"\n\
             q = users & order [desc .created_at, asc .id] & limit 5\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("ORDER BY q.\"created_at\" DESC, q.\"id\""));
    assert!(q.sql.contains("LIMIT 5"));
}

#[test]
fn compiles_a_named_limit_count() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             n = 5\n\
             q = users & limit n\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("LIMIT 5"));
}

#[test]
fn rejects_a_non_literal_limit_count_at_compile_time() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             q = users & limit (id => id)\n",
    )
    .expect("parse");
    assert!(compile(&program).is_err());
}

#[test]
fn compiles_computed_select_projections() {
    let program = parse(
        "orders : query { price = float, qty = int } = table \"public\" \"orders\"\n\
             q = orders & select { total = .price * .qty, qty = .qty }\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"price\" * q.\"qty\" AS \"total\""));
    assert!(q.sql.contains("q.\"qty\" AS \"qty\""));
}
