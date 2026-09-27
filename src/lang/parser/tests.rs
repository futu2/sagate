use super::*;
use crate::lang::type_check;

#[test]
fn prelude_defines_functional_helpers_in_sagate() {
    let program = parse(
        "constant_value : int = constant 7 \"ignored\"\n\
             increment : int -> int = value => value + 1\n\
             composed_value : int = compose increment increment constant_value\n\
             pair : int -> string -> int = first => second => first\n\
             flipped : string -> int -> int = flip pair\n\
             flipped_value : int = flipped \"ignored\" 9\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn sql_templates_require_a_function_signature() {
    let program = parse("lower = sql \"LOWER($1)\"\n").expect("parse");
    let error = type_check(&program).expect_err("template needs a signature");
    assert!(error
        .to_string()
        .contains("SQL template requires a function type signature"));
}

#[test]
fn parses_forward_application_as_an_ordinary_infix_application() {
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
    let rows = type_check(&program).expect("type check");
    let row = rows.get("active").expect("query row");
    assert_eq!(row.field("name").expect("name").ty, Type::String);
    let active = program
        .bindings
        .iter()
        .find(|binding| binding.name == "active")
        .expect("active binding");
    assert!(matches!(active.expr, Expr::Apply { .. }));
}

#[test]
fn application_and_composition_operators_are_available() {
    let program = parse(
        "users : query { id = Int, active = Bool } = table \"public\" \"users\"\n\
             q = ((where (.active == true)) >>> (select { id = .id })) $ users\n\
             q2 = ((select { id = .id }) <<< (where (.active == true))) $ users\n",
    )
    .expect("parse");
    assert!(program.bindings.iter().any(|binding| binding.name == "&"));
    assert!(program.bindings.iter().any(|binding| binding.name == "$"));
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("id").unwrap().ty, Type::Int);
    assert_eq!(rows["q2"].field("id").unwrap().ty, Type::Int);
    let compiled = crate::sql::compile(&program).expect("compile");
    let q = compiled.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"active\" = TRUE"));
    assert!(q.sql.contains("q.\"id\" AS \"id\""));
    let q2 = compiled.iter().find(|query| query.name == "q2").unwrap();
    assert!(q2.sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn core_application_operators_are_available_as_sections() {
    let program = parse("forward = _&_ 1 (x => x)\napply = _$_ (x => x) 1\n").expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn forward_application_remains_an_infix_ast_node() {
    let program = parse("value = 1 & id\napplied = id $ 1\n").expect("parse");
    type_check(&program).expect("type check");
    let binding = program
        .bindings
        .iter()
        .find(|binding| binding.name == "value")
        .expect("value binding");
    let Expr::Apply { function, .. } = &binding.expr else {
        panic!("expected an infix application")
    };
    let Expr::Apply { function, .. } = function.as_ref() else {
        panic!("expected an operator application")
    };
    assert!(matches!(function.as_ref(), Expr::Var(name) if name == "&"));

    let binding = program
        .bindings
        .iter()
        .find(|binding| binding.name == "applied")
        .expect("applied binding");
    let Expr::Apply { function, .. } = &binding.expr else {
        panic!("expected an infix application")
    };
    let Expr::Apply { function, .. } = function.as_ref() else {
        panic!("expected an operator application")
    };
    assert!(matches!(function.as_ref(), Expr::Var(name) if name == "$"));
}

#[test]
fn removed_pipeline_token_is_rejected() {
    assert!(parse(
        "users : query { id = Int } = table \"public\" \"users\"\nq = users |> select { id = .id }\n"
    )
    .is_err());
}

#[test]
fn primitives_are_declared_by_the_prelude_only() {
    // Users cannot define double-underscore names.
    assert!(parse("__where = predicate => relation => relation\n").is_err());
    assert!(parse("__count : a -> agg int;\n").is_err());
    // Users cannot reference an undeclared primitive.
    assert!(parse("q = __bogus 1 2\n").is_err());
    // Declared primitives remain referenceable, e.g. for custom combinators.
    assert!(parse("q = __snake\n").is_ok());
}

#[test]
fn merge_is_right_biased() {
    let older = Row::new(vec![Column {
        name: "id".into(),
        ty: Type::Int,
    }]);
    let newer = Row::new(vec![Column {
        name: "id".into(),
        ty: Type::String,
    }]);
    assert_eq!(older.merge(&newer).field("id").unwrap().ty, Type::String);
}

#[test]
fn first_class_prelude_function_can_be_bound_and_piped() {
    let program = parse(
        "users : query { id = Int, displayName = String } = table \"public\" \"users\";\n\
             let snake = mapKey(snake);\
             q = users & snake & select { id = .id };",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("id").unwrap().ty, Type::Int);
}

#[test]
fn lambda_predicate_is_a_function_value() {
    let program = parse(
        "users : query { id = Int, active = Bool } = table \"public\" \"users\";\n\
             q = where(row => row.active == true, users);",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("active").unwrap().ty, Type::Bool);
}

#[test]
fn implicit_field_syntax_desugars_to_lambdas() {
    let program = parse(
        "is_active = (.active == true)\n\
         projection = { id = .id }\n\
         matches = (.user_id == that.owner_id)\n",
    )
    .expect("parse");

    let is_active = program
        .bindings
        .iter()
        .find(|binding| binding.name == "is_active")
        .unwrap();
    assert!(matches!(is_active.expr, Expr::Lambda { .. }));

    let projection = program
        .bindings
        .iter()
        .find(|binding| binding.name == "projection")
        .unwrap();
    assert!(matches!(projection.expr, Expr::Lambda { .. }));

    let matches = program
        .bindings
        .iter()
        .find(|binding| binding.name == "matches")
        .unwrap();
    let Expr::Lambda { body, .. } = &matches.expr else {
        panic!("expected the two-row shorthand to be a lambda")
    };
    assert!(matches!(body.as_ref(), Expr::Lambda { .. }));
}

#[test]
fn implicit_two_row_join_syntax_type_checks() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
         orders : query { owner_id = int } = table \"public\" \"orders\"\n\
         report = orders & inner users (.owner_id == that.id)\n",
    )
    .expect("parse");
    let row = type_check(&program).expect("type check")["report"].clone();
    assert_eq!(row.field("id").unwrap().ty, Type::Int);
    assert_eq!(row.field("owner_id").unwrap().ty, Type::Int);
}

#[test]
fn table_is_a_curried_source_function_with_an_open_row() {
    let program = parse("q = table(\"public\", \"users\") & select { id = .id };").expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("id").unwrap().ty, Type::Any);
}

#[test]
fn supports_annotated_bindings_and_whitespace_application() {
    let program = parse(
        r#"
            users : query { id = int, active = bool } = table "public" "users";
            active : query { id = int } =
              users & where (.active == true) & select { id = .id };
            "#,
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["users"].field("id").unwrap().ty, Type::Int);
    assert_eq!(rows["active"].field("id").unwrap().ty, Type::Int);
}

#[test]
fn supports_annotations_inside_lambda_parameters() {
    let program = parse(
        "users : query { active = Bool } = table \"public\" \"users\"\n\
             q = where (row : { active = Bool } => row.active == true) (users);",
    )
    .expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn supports_lambda_syntax() {
    let program = parse(
        "is_active = row : { active = bool } => row.active == true;\n\
             users : query { active = bool } = table \"public\" \"users\";\n\
             q = users & where is_active;",
    )
    .expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn fn_lambda_syntax_is_rejected() {
    assert!(parse("value = fn row => row\n").is_err());
}

#[test]
fn definitions_are_separated_by_newlines_without_semicolons() {
    let program = parse(
            "users : query { id = int } = table \"public\" \"users\"\nactive = users & select { id = .id }\n",
        )
        .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["active"].field("id").unwrap().ty, Type::Int);
}

#[test]
fn infix_operator_sections_are_function_applications() {
    let program = parse(
        "_+_ : int -> int -> int = x => y => x\n\
             left = _+_ 1 2\n\
             right = 1 + 2\n",
    )
    .expect("parse");
    let left = program
        .bindings
        .iter()
        .find(|binding| binding.name == "left")
        .expect("left binding");
    let right = program
        .bindings
        .iter()
        .find(|binding| binding.name == "right")
        .expect("right binding");

    assert_eq!(left.expr, right.expr);
    type_check(&program).expect("type check");
}

#[test]
fn overloaded_arithmetic_selects_a_concrete_numeric_type() {
    let program = parse(
        "integer = 1 + 2\n\
             decimal = 1.0 + 2.0\n\
             difference = 4.0 - 1.5\n\
             plus_one = _+_ 1\n\
             applied = plus_one 2\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");

    let bad = parse("mixed = 1 + 2.0\n").expect("parse");
    assert!(type_check(&bad).is_err());
}

#[test]
fn annotated_named_definitions_can_be_overloaded() {
    let program = parse(
        "choose : int -> int = value => value\n\
             choose : float -> float = value => value\n\
             integer = choose 1\n\
             decimal = choose 1.0\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");

    let bad = parse(
        "choose : int -> int = value => value\n\
             choose : float -> float = value => value\n\
             invalid = choose true\n",
    )
    .expect("parse");
    assert!(type_check(&bad).is_err());
}

#[test]
fn query_signatures_keep_named_row_variables() {
    let program = parse("value = 1\n").expect("parse");
    let select = program
        .bindings
        .iter()
        .find(|binding| binding.name == "select")
        .expect("select prelude binding");
    let Some(Type::Function(projection, result)) = &select.annotation else {
        panic!("expected a curried select signature")
    };
    assert!(matches!(projection.as_ref(), Type::Function(_, _)));
    let Type::Function(projection_input, projection_output) = projection.as_ref() else {
        unreachable!()
    };
    let Type::RowType(projection_input_row) = projection_input.as_ref() else {
        panic!("expected projection to consume a row value type")
    };
    let Type::RowType(projection_output_row) = projection_output.as_ref() else {
        panic!("expected projection to return a row value type")
    };
    let RowExpr::Variable(projection_input_id) = projection_input_row.as_ref() else {
        panic!("expected projection input row variable")
    };
    let RowExpr::Variable(projection_output_id) = projection_output_row.as_ref() else {
        panic!("expected projection output row variable")
    };
    let Type::Function(input, output) = result.as_ref() else {
        panic!("expected select's relation arguments")
    };
    let Type::RelationExpr(input_row) = input.as_ref() else {
        panic!("expected an input row variable")
    };
    let Type::RelationExpr(output_row) = output.as_ref() else {
        panic!("expected an output row variable")
    };
    let RowExpr::Variable(input_id) = input_row.as_ref() else {
        panic!("expected a named input row")
    };
    let RowExpr::Variable(output_id) = output_row.as_ref() else {
        panic!("expected a named output row")
    };
    assert_eq!(projection_input_id, input_id);
    assert_eq!(projection_output_id, output_id);
    assert_ne!(input_id, output_id);

    let where_binding = program
        .bindings
        .iter()
        .find(|binding| binding.name == "where")
        .expect("where prelude binding");
    let Some(Type::Function(predicate, result)) = &where_binding.annotation else {
        panic!("expected a curried where signature")
    };
    let Type::Function(predicate_row, predicate_result) = predicate.as_ref() else {
        panic!("where predicate should be an ordinary function")
    };
    let Type::RowType(predicate_row_type) = predicate_row.as_ref() else {
        panic!("where predicate should consume a row value type")
    };
    let RowExpr::Variable(predicate_row_id) = predicate_row_type.as_ref() else {
        panic!("where predicate should consume a row variable")
    };
    assert_eq!(predicate_result.as_ref(), &Type::Bool);
    let Type::Function(where_input, _) = result.as_ref() else {
        panic!("expected where's relation arguments")
    };
    let Type::RelationExpr(where_row) = where_input.as_ref() else {
        panic!("where should take a query over its predicate row")
    };
    assert_eq!(where_row.as_ref(), &RowExpr::Variable(*predicate_row_id));
}

#[test]
fn mapper_signatures_express_output_rows_as_type_level_operations() {
    let program = parse("value = 1\n").expect("parse");
    let map_key = program
        .bindings
        .iter()
        .find(|b| b.name == "mapKey")
        .unwrap();
    let Type::Function(mapper, result) = map_key.annotation.as_ref().unwrap() else {
        panic!("expected a curried mapKey signature")
    };
    let Type::KeyMapperWitness(mapper_argument) = mapper.as_ref() else {
        panic!("expected a mapper type variable")
    };
    let MapperType::Variable(mapper_id) = mapper_argument.as_ref() else {
        panic!("expected the mapper variable")
    };
    let Type::Function(input, output) = result.as_ref() else {
        panic!("expected mapKey relation arguments")
    };
    let Type::RelationExpr(input_row) = input.as_ref() else {
        panic!("expected mapKey's input row variable")
    };
    let RowExpr::Variable(input_id) = input_row.as_ref() else {
        panic!("expected mapKey's named input row")
    };
    let Type::RelationExpr(output_row) = output.as_ref() else {
        panic!("expected mapKey's output to be a row expression")
    };
    let RowExpr::MapKey(output_mapper, row) = output_row.as_ref() else {
        panic!("expected mapkey in the result row")
    };
    assert_eq!(row.as_ref(), &RowExpr::Variable(*input_id));
    assert_eq!(output_mapper.as_ref(), &MapperType::Variable(*mapper_id));

    let merge = program.bindings.iter().find(|b| b.name == "merge").unwrap();
    let Type::Function(_, result) = merge.annotation.as_ref().unwrap() else {
        panic!("expected a curried merge signature")
    };
    let Type::Function(_, output) = result.as_ref() else {
        panic!("expected merge's second relation argument")
    };
    assert!(matches!(
        output.as_ref(),
        Type::RelationExpr(row) if matches!(row.as_ref(), RowExpr::Merge(_, _))
    ));
}

#[test]
fn row_operations_normalize_against_concrete_annotations() {
    let good = parse(
            "users : query { displayName = string, age = int } = table \"public\" \"users\"\n             renamed : query { display_name = string, age = int } = users & mapKey snake\n             nullable : query { displayName = maybe string, age = maybe int } = users & mapValue maybe\n             merged : query { displayName = string, age = int } = merge users users\n",
        )
        .expect("parse");
    type_check(&good).expect("type check");

    let bad_key = parse(
            "users : query { displayName = string } = table \"public\" \"users\"\n             renamed : query { displayName = string } = users & mapKey snake\n",
        )
        .expect("parse");
    assert!(type_check(&bad_key).is_err());

    let bad_merge = parse(
            "users : query { displayName = string } = table \"public\" \"users\"\n             other : query { age = int } = table \"public\" \"other\"\n             merged : query { displayName = string } = merge users other\n",
        )
        .expect("parse");
    assert!(type_check(&bad_merge).is_err());
}

#[test]
fn row_operator_kinds_reject_cross_axis_mappers() {
    let key_with_value_mapper =
        parse("bad : query (mapkey maybe r) -> query r = relation => relation\n").expect("parse");
    let error = type_check(&key_with_value_mapper).expect_err("kind mismatch");
    assert!(error.to_string().contains("mapkey expects a key mapper"));

    let value_with_key_mapper =
        parse("bad : query (mapvalue snake r) -> query r = relation => relation\n").expect("parse");
    let error = type_check(&value_with_key_mapper).expect_err("kind mismatch");
    assert!(error
        .to_string()
        .contains("mapvalue expects a value mapper"));

    let nested = parse(
            "ok : query (mapkey snake (merge r s)) -> query (mapkey snake (merge r s)) = value => value\n",
        )
        .expect("nested row terms parse");
    type_check(&nested).expect("nested row terms type check");
}

#[test]
fn row_polymorphic_definitions_preserve_and_validate_rows() {
    let program = parse(
        "users : query { id = int, active = bool } = table \"public\" \"users\"\n\
             keep : query r -> query r = relation => relation\n\
             q = keep users\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("id").unwrap().ty, Type::Int);
    assert_eq!(rows["q"].field("active").unwrap().ty, Type::Bool);

    let bad = parse(
        "bad : query r -> query r = relation => true\n\
             q = bad (table \"public\" \"users\")\n",
    )
    .expect("parse");
    assert!(type_check(&bad).is_err());

    let mismatched_row_function =
        parse("invalid : { id = int } -> { name = string } = row => row\n")
            .expect("parse ordinary row function");
    assert!(type_check(&mismatched_row_function).is_err());
}

#[test]
fn type_variables_are_distinct_inside_a_signature() {
    let program = parse(
        "first : a -> b -> a = left => right => left\n\
             value = first 1 true\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");

    let same = parse(
        "same : a -> a -> a = left => right => left\n\
             invalid = same 1 true\n",
    )
    .expect("parse");
    assert!(type_check(&same).is_err());
}

#[test]
fn polymorphic_definitions_are_instantiated_per_use() {
    let program = parse(
        "identity2 : a -> a = value => value\n\
             number = identity2 1\n\
             flag = identity2 true\n",
    )
    .expect("parse");
    type_check(&program).expect("each use gets a fresh type variable");
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
    let compiled = crate::sql::compile(&program).expect("compile");
    let q = compiled.iter().find(|query| query.name == "q").unwrap();
    assert!(q.sql.contains("q.\"active\" = TRUE"));
}

#[test]
fn local_and_multicharacter_operator_sections_are_supported() {
    let program = parse("value = let _++_ = x => y => x in _++_ 1 2\n").expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn row_constructor_marks_row_kind_and_rejects_kind_mismatch() {
    let good = parse("keep : row r -> row r = value => value\n").expect("parse");
    type_check(&good).expect("row constructor should make the arrow well-kinded");

    let bad = parse("invalid : r -> query r = value => value\n").expect("parse");
    let error = type_check(&bad).expect_err("one variable cannot have Type and Row kinds");
    assert!(error.to_string().contains("used at kinds"));
}

#[test]
fn prelude_has_scalar_temporal_and_aggregate_types() {
    let program = parse(
            "events_q : query { id = int, happened = timestamp, day = date, amount = float } = table \"public\" \"events\"\n\
             totals = events_q & agg { day = group .day, total = sum .amount, rows = count }\n",
        )
        .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["totals"].field("day").unwrap().ty, Type::Date);
    assert_eq!(rows["totals"].field("total").unwrap().ty, Type::Float);
    assert_eq!(rows["totals"].field("rows").unwrap().ty, Type::Int);
}

#[test]
fn table_rows_come_from_the_binding_annotation() {
    let program = parse(
        "users : query { id = int, active = bool } = table \"public\" \"users\"\n\
             q = users & where (.active == true)\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("active").unwrap().ty, Type::Bool);
}

#[test]
fn table_rows_accept_an_inline_type_annotation() {
    let program = parse(
        "users = table \"public\" \"users\" : query { id = int, name = string }\n\
             q = users & select { name = .name }\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("name").unwrap().ty, Type::String);
}

#[test]
fn table_rows_stay_open_until_a_binding_refines_them() {
    let program = parse(
        "users = table \"public\" \"users\"\n\
             q = users & select { id = .id }\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("id").unwrap().ty, Type::Any);
}

#[test]
fn rejects_mismatched_scalar_and_aggregate_types() {
    let bad_operator = parse("value = true + 1\n").expect("parse");
    assert!(type_check(&bad_operator).is_err());

    let bad_comparison = parse("value = true == 1\n").expect("parse");
    assert!(type_check(&bad_comparison).is_err());

    let bad_aggregate = parse(
        "users : query { name = string } = table \"public\" \"users\"\n\
             bad = users & agg { total = sum .name }\n",
    )
    .expect("parse");
    assert!(type_check(&bad_aggregate).is_err());

    let unknown_field = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             bad = users & agg { total = sum .missing }\n",
    )
    .expect("parse");
    assert!(type_check(&unknown_field).is_err());
}

#[test]
fn join_checks_key_types_and_outer_join_nullability() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             orders : query { user_id = int, total = float } = table \"public\" \"orders\"\n\
             report = users & left orders (l => r => l.id == r.user_id)\n",
    )
    .expect("parse");
    let row = &type_check(&program).expect("type check")["report"];
    assert_eq!(row.field("id").unwrap().ty, Type::Int);
    assert_eq!(
        row.field("total").unwrap().ty,
        Type::Maybe(Box::new(Type::Float))
    );

    let bad = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             orders : query { user_id = string } = table \"public\" \"orders\"\n\
             report = users & inner orders (l => r => l.id == r.user_id)\n",
    )
    .expect("parse");
    assert!(type_check(&bad).is_err());
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
    let queries = crate::sql::compile(&program).expect("compile");
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
    let queries = crate::sql::compile(&program).expect("compile");
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
    let queries = crate::sql::compile(&program).expect("compile");
    let query = queries.iter().find(|query| query.name == "q").unwrap();
    assert!(query.sql.contains("COUNT(q.\"id\") AS \"rows\""));
    assert!(query.sql.contains("COUNT(*) AS \"all_rows\""));
}

#[test]
fn comparison_operator_uses_prelude_binding_in_predicates() {
    let program = parse(
        "_==_ = left => right => __ne left right\n\
             users : query { active = bool } = table \"public\" \"users\"\n\
             q = users & where (.active == true)\n\
             r = users & where (row => row.active == true)\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");
    let queries = crate::sql::compile(&program).expect("compile");
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
    let queries = crate::sql::compile(&program).expect("compile");
    assert!(queries
        .iter()
        .any(|query| query.sql.contains("AS \"user_id\"")));
    assert!(queries
        .iter()
        .any(|query| query.sql.contains("AS \"id_column\"")));
}

#[test]
fn temporal_literals_are_syntax() {
    let program = parse(
        "day : date = date \"2026-01-01\"\n\
             moment : timestamp = timestamp \"2026-01-01T00:00:00Z\"\n",
    )
    .expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn order_and_limit_type_check_against_the_relation_row() {
    let program = parse(
        r#"
            users : query { id = Int, lastName = String, age = Int } = table "public" "users"
            by_age = users & order [asc .age] & limit 10
            by_name = users & order [asc .lastName, desc .age]
            explicit = users & order [asc .lastName, desc .age]
            "#,
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["by_age"].field("age").unwrap().ty, Type::Int);
    assert_eq!(rows["by_name"].field("lastName").unwrap().ty, Type::String);
    assert_eq!(rows["explicit"].field("id").unwrap().ty, Type::Int);
}

#[test]
fn spaced_dot_starts_a_new_argument_not_field_access() {
    let program = parse(
        "users : query { id = Int, name = String } = table \"public\" \"users\"\n\
             q = users & order [desc .name]\n\
             r = users & order (row => [asc(row.name)])\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(rows["q"].field("name").unwrap().ty, Type::String);
    assert_eq!(rows["r"].field("name").unwrap().ty, Type::String);
}

#[test]
fn order_preserves_the_row_of_its_relation() {
    let program = parse(
        "users : query { id = Int, name = Maybe String } = table \"public\" \"users\"\n\
             q = users & order [desc .name]\n",
    )
    .expect("parse");
    let rows = type_check(&program).expect("type check");
    assert_eq!(
        rows["q"].field("name").unwrap().ty,
        Type::Maybe(Box::new(Type::String))
    );
}

#[test]
fn order_rejects_raw_values_as_keys() {
    let program =
        parse("users : query { id = Int } = table \"public\" \"users\"\nq = users & order [1]\n")
            .expect("parse");
    let error = type_check(&program).expect_err("order keys must be directions");
    assert!(error.to_string().contains("order"), "got: {error}");
}

#[test]
fn limit_rejects_a_non_literal_count() {
    let program = parse(
        "users : query { id = Int } = table \"public\" \"users\"\nq = users & limit (row => row.id)\n",
    )
    .expect("parse");
    let error = type_check(&program).expect_err("limit needs a count");
    assert!(error.to_string().contains("limit"), "got: {error}");
}

#[test]
fn row_literals_construct_row_values() {
    let program =
        parse("point : row { x = int, label = string } = { x = 1, label = \"origin\" }\n")
            .expect("parse");
    type_check(&program).expect("type check");
}

#[test]
fn row_literal_fields_must_exist_on_the_row() {
    let program = parse(
        "users : query { id = int } = table \"public\" \"users\"\n\
             q = users & select { name = .name }\n",
    )
    .expect("parse");
    let error = type_check(&program).expect_err("unknown field");
    assert!(error.to_string().contains("name"), "got: {error}");
}
