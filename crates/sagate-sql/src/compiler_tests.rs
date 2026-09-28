use super::*;
use sagate_core::lang::Expr;
use sagate_core::lang::{parse, type_check, Type};

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
fn compiles_sql_functions_defined_in_the_prelude() {
    let program = parse(
        "users : query { name = string } = table \"public\" \"users\"\n\
             q = users & select (row => { normalized = lower(row.name), name_length = length(row.name) })\n",
    )
    .expect("parse");
    let queries = compile(&program).expect("compile");
    let q = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("LOWER(q.\"name\") AS \"normalized\""));
    assert!(q.sql.contains("LENGTH(q.\"name\") AS \"name_length\""));
}

#[test]
fn compiles_multi_argument_sql_templates() {
    let program = parse(
        "between : int -> int -> int -> bool = sql \"$1 BETWEEN $2 AND $3\"\n\
             users : query { age = int } = table \"public\" \"users\"\n\
             q = users & select (row => { adult = between (row.age) 18 65 })\n",
    )
    .expect("parse");
    let queries = compile(&program).expect("compile");
    let q = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"age\" BETWEEN 18 AND 65"), "{}", q.sql);
}

#[test]
fn rejects_sql_templates_with_missing_arguments() {
    let program = parse(
        "label : string -> string = sql \"UPPER($2)\"\n\
             users : query { name = string } = table \"public\" \"users\"\n\
             q = users & select (row => { label = label(row.name) })\n",
    )
    .expect("parse");
    let error = compile(&program).expect_err("template arity mismatch");
    assert!(
        error.contains("SQL template expects 2 positional arguments"),
        "{error}"
    );
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

#[test]
fn compiles_unicode_names_with_quoted_identifiers() {
    let program = parse(
        r#"
            利用者 : query { 名前 = string, 年齢 = int } = table "public" "users"
            一覧 = 利用者
              & where (.年齢 >= 18)
              & select { 名前 = .名前, 大人 = true }
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let query = sql
        .iter()
        .find(|query| query.name == "一覧")
        .expect("Unicode binding name is reported");
    assert!(query.sql.contains("q.\"年齢\" >= 18"), "{}", query.sql);
    assert!(
        query.sql.contains("q.\"名前\" AS \"名前\""),
        "{}",
        query.sql
    );
}

#[test]
fn compiles_unicode_table_and_schema_names() {
    let program = parse(
        "q : query { 名前 = string } = table \"公開\" \"利用者\" & select { 名前 = .名前 }\n",
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    assert!(
        sql[0].sql.contains("FROM \"公開\".\"利用者\""),
        "{}",
        sql[0].sql
    );
}

#[test]
fn unicode_field_survives_mapkey_snake_and_aliases() {
    // snake/kebab/camel only rewrite ASCII case and separators; the Unicode
    // label passes through untouched, and the projection still resolves.
    let program = parse(
        r#"
            users : query { 名前 = string, firstName = string } = table "public" "users"
            q = users
              & mapKey snake
              & select { 名前 = .名前, first_name = .first_name }
            "#,
    )
    .expect("parse");
    let sql = compile(&program).expect("compile");
    let q = sql.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"名前\" AS \"名前\""), "{}", q.sql);
    assert!(
        q.sql.contains("q.\"first_name\" AS \"first_name\""),
        "{}",
        q.sql
    );
}

#[test]
fn comparison_operators_are_function_applications_and_sql_predicates() {
    let program = parse(
        "users : query { active = bool } = table \"public\" \"users\"\n\
             is_active = row : { active = bool } => row.active == true\n\
             q = users & where is_active\n",
    )
    .expect("parse");
    let is_active = program
        .bindings
        .iter()
        .find(|binding| binding.name == "is_active")
        .expect("predicate binding");
    let Expr::Lambda { body, .. } = &is_active.expr else {
        panic!("expected a lambda")
    };
    assert!(matches!(body.as_ref(), Expr::Apply { .. }));
    type_check(&program).expect("type check");
    let compiled = crate::compile_program(&program).expect("compile");
    let q = compiled.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn prelude_override_controls_relation_semantics() {
    let program = parse(
        "users : query { id = int, active = bool } = table \"public\" \"users\"\n\
             select = projection => relation => __where (.active == true) relation\n\
             q = users & select { id = .id }\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert!(rows["q"].field("active").is_some());
    let queries = crate::compile_program(&program).expect("compile");
    let query = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(query.sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn user_alias_can_take_an_aggregate_projection() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             summarize = agg\n\
             q = users & summarize { rows = count }\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("rows").unwrap().ty, Type::Int);
    let queries = crate::compile_program(&program).expect("compile");
    let query = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(query.sql.contains("COUNT(*)"));
}

#[test]
fn aggregate_constructor_uses_prelude_binding() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             sum = field => __count field\n\
             countRows = count\n\
             q = users & agg { rows = sum .id, all_rows = countRows }\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("rows").unwrap().ty, Type::Int);
    assert_eq!(rows["q"].field("all_rows").unwrap().ty, Type::Int);
    let queries = crate::compile_program(&program).expect("compile");
    let query = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(query.sql.contains("COUNT(q.\"id\") AS \"rows\""));
    assert!(query.sql.contains("COUNT(*) AS \"all_rows\""));
}

#[test]
fn comparison_operator_uses_prelude_binding_in_predicates() {
    let program = parse(
        "_==_ = left => right => left != right\n\
             users : query { active = bool } = table \"public\" \"users\"\n\
             q = users & where (.active == true)\n\
             r = users & where (row => row.active == true)\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");
    let queries = crate::compile_program(&program).expect("compile");
    for name in ["q", "r"] {
        let query = queries.iter().find(|query| query.name == name).unwrap();
        assert!(query.sql.contains("q.\"active\" <> TRUE"));
    }
}

#[test]
fn mapper_constructors_are_prelude_functions() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             q = users & mapKey (prefix \"user_\")\n\
             r = users & mapKey(suffix(\"_column\"))\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert!(rows["q"].field("user_id").is_some());
    assert!(rows["r"].field("id_column").is_some());
    let queries = crate::compile_program(&program).expect("compile");
    assert!(queries
        .iter()
        .any(|query| query.sql.contains("AS \"user_id\"")));
    assert!(queries
        .iter()
        .any(|query| query.sql.contains("AS \"id_column\"")));
}
