//! Module-system tests: file loading, visibility, resolution, and SQL output.
//!
//! Each test writes a small multi-file project into a fresh temporary
//! directory and compiles its entry file through `link_file`, so the loader's
//! path handling, cycle detection, and diagnostics are exercised end to end.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::lang::link_file;
use crate::sql::{compile_linked_with_dialect, CompiledQuery};

/// A throwaway project directory that removes itself on drop.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        // A per-test counter keeps parallel tests from sharing a directory.
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sagate-module-test-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create project directory");
        Self { root }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(&path, source).expect("write source file");
        path
    }

    fn compile(&self, entry: &str) -> Result<Vec<CompiledQuery>, String> {
        self.compile_dialect(entry, "ansi")
    }

    fn compile_dialect(&self, entry: &str, dialect: &str) -> Result<Vec<CompiledQuery>, String> {
        let path = self.root.join(entry);
        let linked = link_file(&path)?;
        compile_linked_with_dialect(&linked, dialect)
    }

    fn error(&self, entry: &str) -> String {
        self.compile(entry).expect_err("expected a failure")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn query<'a>(queries: &'a [CompiledQuery], name: &str) -> &'a CompiledQuery {
    queries
        .iter()
        .find(|query| query.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no query named '{name}'; got {:?}",
                queries.iter().map(|query| &query.name).collect::<Vec<_>>()
            )
        })
}

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The manual example from the module-system design document compiles to the
/// expected SQL, with dependency queries absent from the entry output.
#[test]
fn compiles_an_imported_query_and_function() {
    let project = Project::new("design-example");
    project.write(
        "models/users.sagate",
        "users : query { id = int, name = string, active = bool } =\n\
         \x20 table \"public\" \"users\"\n\
         \n\
         export active_users = users & where (.active == true)\n\
         \n\
         export normalize : string -> string = sql \"LOWER($1)\"\n",
    );
    project.write(
        "report.sagate",
        "import { active_users, normalize as normalize_name } from \"./models/users.sagate\"\n\
         \n\
         report = active_users\n\
         \x20 & select (row => { id = row.id, name = normalize_name(row.name) })\n",
    );

    let queries = project.compile("report.sagate").expect("compile report");
    // Only the entry file's local query produces output.
    assert_eq!(queries.len(), 1, "got {:?}", queries);
    let report = query(&queries, "report");
    assert!(report.sql.contains("q.\"active\" = TRUE"), "{}", report.sql);
    assert!(
        report.sql.contains("LOWER(q.\"name\") AS \"name\""),
        "{}",
        report.sql
    );
    assert!(!queries.iter().any(|query| query.name == "users"));
    assert!(!queries.iter().any(|query| query.name == "active_users"));
}

/// Compiling a dependency file directly emits its locally defined queries,
/// exported or not, in definition order.
#[test]
fn compiling_a_dependency_emits_its_local_queries() {
    let project = Project::new("dependency-entry");
    project.write(
        "models/users.sagate",
        "users : query { id = int, active = bool } = table \"public\" \"users\"\n\
         export active_users = users & where (.active == true)\n",
    );
    let queries = project
        .compile("models/users.sagate")
        .expect("compile dependency");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(names, vec!["users", "active_users"]);
}

/// Existing single-file behavior is unchanged: local queries keep their
/// names, order, and SQL whether or not the file uses module declarations.
#[test]
fn single_file_programs_keep_their_output() {
    let project = Project::new("single-file");
    project.write(
        "one.sagate",
        "users : query { id = int, name = string } = table \"public\" \"users\"\n\
         active = users & where (.id > 0)\n",
    );
    let queries = project.compile("one.sagate").expect("compile");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(names, vec!["users", "active"]);
    assert!(query(&queries, "active").sql.contains("q.\"id\" > 0"));
}

/// Importing a private name fails with the written path and line.
#[test]
fn importing_a_private_name_fails() {
    let project = Project::new("private-import");
    project.write(
        "models.sagate",
        "users : query { id = int } = table \"public\" \"users\"\n\
         export active = users\n",
    );
    project.write(
        "report.sagate",
        "import { users } from \"./models.sagate\"\nq = users\n",
    );
    let error = project.error("report.sagate");
    assert!(error.contains("report.sagate:1"), "{error}");
    assert!(
        error.contains("'./models.sagate' does not export 'users'"),
        "{error}"
    );
}

/// A private helper stays reachable inside its own module even when the
/// importer defines a helper with the same name.
#[test]
fn imported_functions_keep_their_private_helpers() {
    let project = Project::new("private-helper");
    project.write(
        "models.sagate",
        "helper : string -> string = sql \"LOWER($1)\"\n\
         export loud : string -> string = name => helper(name)\n\
         export quiet : string -> string = helper\n",
    );
    project.write(
        "report.sagate",
        "import { loud, quiet } from \"./models.sagate\"\n\
         helper : string -> string = sql \"UPPER($1)\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { a = loud(row.name), b = quiet(row.name), c = helper(row.name) })\n",
    );
    let queries = project.compile("report.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // The imported function resolves to its own module's private helper.
    assert!(sql.contains("LOWER(q.\"name\") AS \"a\""), "{sql}");
    assert!(sql.contains("LOWER(q.\"name\") AS \"b\""), "{sql}");
    // The importer's own helper keeps the local spelling.
    assert!(sql.contains("UPPER(q.\"name\") AS \"c\""), "{sql}");
}

/// Two modules may define the same private name without conflict.
#[test]
fn private_names_do_not_collide_across_modules() {
    let project = Project::new("private-collision");
    project.write(
        "left.sagate",
        "helper : string -> string = sql \"LOWER($1)\"\n\
         export via_left : string -> string = name => helper(name)\n",
    );
    project.write(
        "right.sagate",
        "helper : string -> string = sql \"UPPER($1)\"\n\
         export via_right : string -> string = name => helper(name)\n",
    );
    project.write(
        "main.sagate",
        "import { via_left } from \"./left.sagate\"\n\
         import { via_right } from \"./right.sagate\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { l = via_left(row.name), r = via_right(row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("LOWER(q.\"name\") AS \"l\""), "{sql}");
    assert!(sql.contains("UPPER(q.\"name\") AS \"r\""), "{sql}");
}

/// A prelude override applies inside its module only; other modules and the
/// re-exported function keep the original prelude meaning.
#[test]
fn prelude_overrides_stay_within_their_module() {
    let project = Project::new("prelude-override");
    project.write(
        "arithmetic.sagate",
        "_+_ : int -> int -> int = sql \"$1 - $2\"\n\
         export subtractish : int -> int -> int = a => b => a + b\n",
    );
    project.write(
        "main.sagate",
        "import { subtractish } from \"./arithmetic.sagate\"\n\
         users : query { id = int } = table \"public\" \"users\"\n\
         q = users & select (row => { d = subtractish(row.id) 2, s = row.id + 2 })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // The dependency's `+` became `-`; the entry file's `+` is untouched.
    assert!(sql.contains("q.\"id\" - 2 AS \"d\""), "{sql}");
    assert!(sql.contains("q.\"id\" + 2 AS \"s\""), "{sql}");
}

/// Overriding a prelude binding that other prelude definitions alias keeps
/// the shared prelude resolved: `id` and `compose` still refer to the
/// original `identity` and `>>>`, while the module's own later definitions
/// see the override.
#[test]
fn overriding_a_prelude_alias_keeps_prelude_bindings_resolved() {
    let project = Project::new("prelude-alias-override");
    project.write(
        "main.sagate",
        "identity : int -> int = value => value + 1\n\
         users : query { id = int } = table \"public\" \"users\"\n\
         q = users & select (row => { a = identity (row.id), b = id (row.id), c = compose identity identity (row.id) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // Later definitions in the module see the override.
    assert!(sql.contains("q.\"id\" + 1 AS \"a\""), "{sql}");
    // Prelude `id` still refers to the original identity.
    assert!(sql.contains("q.\"id\" AS \"b\""), "{sql}");
    // Prelude `compose` still refers to the original `>>>`, applied to the override.
    assert!(sql.contains("q.\"id\" + 1 + 1 AS \"c\""), "{sql}");
}

/// Overriding `>>>` reverses composition inside the module while prelude
/// `compose` keeps the original direction.
#[test]
fn overriding_composition_keeps_prelude_compose_resolved() {
    let project = Project::new("compose-override");
    project.write(
        "main.sagate",
        "_>>>_ = first => second => value => first (second value)\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { d = (lower >>> upper) (row.name), c = compose lower upper (row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // The module's `>>>` applies its first argument last.
    assert!(sql.contains("LOWER(UPPER(q.\"name\")) AS \"d\""), "{sql}");
    // Prelude `compose` still refers to the original `>>>`.
    assert!(sql.contains("UPPER(LOWER(q.\"name\")) AS \"c\""), "{sql}");
}

/// Overriding `<<<` applies inside its module only.
#[test]
fn overriding_backwards_composition_stays_within_the_module() {
    let project = Project::new("backcomposition-override");
    project.write(
        "main.sagate",
        "_<<<_ = second => first => value => first (second value)\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { d = (lower <<< upper) (row.name), e = (upper <<< lower) (row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("UPPER(LOWER(q.\"name\")) AS \"d\""), "{sql}");
    assert!(sql.contains("LOWER(UPPER(q.\"name\")) AS \"e\""), "{sql}");
}

/// A prelude override's own body still sees the previous prelude definition,
/// so a self-referential override composes exactly like the original.
#[test]
fn override_bodies_see_the_previous_prelude_definition() {
    let project = Project::new("override-body");
    project.write(
        "main.sagate",
        "_>>>_ = first => second => value => (first >>> second) value\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { d = (lower >>> upper) (row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // The body's `>>>` resolved to the previous prelude definition.
    assert!(sql.contains("UPPER(LOWER(q.\"name\")) AS \"d\""), "{sql}");
}

/// Aliases and re-exports preserve the original binding, including SQL
/// templates and overloaded operators.
#[test]
fn aliases_reexports_and_operators_resolve_to_their_bindings() {
    let project = Project::new("aliases");
    project.write(
        "strings.sagate",
        "export normalize : string -> string = sql \"LOWER($1)\"\n",
    );
    project.write(
        "arithmetic.sagate",
        "export _+_ : int -> int -> int = sql \"$1 + $2\"\n\
         export _*_ : int -> int -> int = sql \"$1 * $2\"\n",
    );
    project.write(
        "models.sagate",
        "import { normalize } from \"./strings.sagate\"\n\
         export { normalize as text }\n",
    );
    project.write(
        "report.sagate",
        "import { text } from \"./models.sagate\"\n\
         import { _+_ as add, _*_ } from \"./arithmetic.sagate\"\n\
         users : query { name = string, age = int } = table \"public\" \"users\"\n\
         q = users & select (row => { n = text(row.name), a = add(row.age) 1, b = row.age * 2 })\n",
    );
    let queries = project.compile("report.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("LOWER(q.\"name\") AS \"n\""), "{sql}");
    assert!(sql.contains("q.\"age\" + 1 AS \"a\""), "{sql}");
    assert!(sql.contains("q.\"age\" * 2 AS \"b\""), "{sql}");
}

/// Row-polymorphic helpers imported from another file behave as the defining
/// binding does, including reaching a private dependency relation.
#[test]
fn imported_row_polymorphic_helpers_work() {
    let project = Project::new("row-poly");
    project.write(
        "tables.sagate",
        "private_users : query { id = int, active = bool } = table \"public\" \"users\"\n\
         export active_only : query { id = int, active = bool } -> query { id = int, active = bool } =\n\
         \x20 where (.active == true)\n\
         export ids_only : query { id = int, active = bool } -> query { id = int } =\n\
         \x20 select (row => { id = row.id })\n\
         export private_source : query { id = int, active = bool } = private_users\n",
    );
    project.write(
        "report.sagate",
        "import { active_only, ids_only, private_source } from \"./tables.sagate\"\n\
         users : query { id = int, active = bool } = table \"public\" \"users\"\n\
         filtered = users & active_only & ids_only\n\
         from_private = private_source & active_only & ids_only\n",
    );
    let queries = project.compile("report.sagate").expect("compile");
    // Both entry queries filter on `active` and project only `id`.
    for name in ["filtered", "from_private"] {
        let sql = &query(&queries, name).sql;
        assert!(sql.contains("q.\"active\" = TRUE"), "{name}: {sql}");
        assert!(sql.contains("AS \"id\""), "{name}: {sql}");
    }
}

/// Lambda parameters and `let` binders shadow imported names inside the
/// defining module, and field labels plus SQL placeholders survive the
/// resolution pass unchanged.
#[test]
fn shadowing_and_labels_survive_resolution() {
    let project = Project::new("shadowing");
    project.write(
        "helpers.sagate",
        "export rename : string -> string = sql \"LOWER($1)\"\n\
         # The lambda parameter shadows the exported `rename` name inside this\n\
         # body, so the reference resolves to the parameter, not the function.\n\
         export passthrough : string -> string = rename => rename\n",
    );
    project.write(
        "main.sagate",
        "import { rename, passthrough } from \"./helpers.sagate\"\n\
         users : query { id = int, name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { id = row.id, lower = rename(row.name), same = passthrough(row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    // The explicit lambda parameter shadows `rename`, so no LOWER wraps it.
    assert!(sql.contains("\"name\" AS \"same\""), "{sql}");
    assert!(!sql.contains("LOWER(q.\"name\") AS \"same\""), "{sql}");
    // Field labels and the imported template are preserved.
    assert!(sql.contains("q.\"id\" AS \"id\""), "{sql}");
    assert!(sql.contains("LOWER(q.\"name\") AS \"lower\""), "{sql}");
}

/// A shared dependency reached through two paths loads once, and imports
/// resolve from the importing file's directory, not the process cwd.
#[test]
fn shared_dependencies_and_relative_paths_resolve() {
    let project = Project::new("diamond");
    project.write(
        "shared/text.sagate",
        "export normalize : string -> string = sql \"LOWER($1)\"\n",
    );
    project.write(
        "left.sagate",
        "import { normalize } from \"./shared/text.sagate\"\n\
         export left : string -> string = name => normalize(name)\n",
    );
    project.write(
        "nested/right.sagate",
        "import { normalize } from \"../shared/text.sagate\"\n\
         export right : string -> string = name => normalize(name)\n",
    );
    project.write(
        "main.sagate",
        "import { left } from \"./left.sagate\"\n\
         import { right } from \"./nested/right.sagate\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { a = left(row.name), b = right(row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("LOWER(q.\"name\") AS \"a\""), "{sql}");
    assert!(sql.contains("LOWER(q.\"name\") AS \"b\""), "{sql}");
}

/// An import cycle fails with the complete cycle in the message.
#[test]
fn import_cycles_are_rejected() {
    let project = Project::new("cycle");
    project.write(
        "a.sagate",
        "import { b } from \"./b.sagate\"\nexport a = b\n",
    );
    project.write(
        "b.sagate",
        "import { a } from \"./a.sagate\"\nexport b = a\n",
    );
    let error = project.error("a.sagate");
    assert!(error.contains("import cycle"), "{error}");
    assert!(
        error.contains("a.sagate -> ./b.sagate -> ./a.sagate"),
        "{error}"
    );
}

/// Missing files, unknown exports, duplicate local names, and forward
/// references all fail with source-facing diagnostics.
#[test]
fn module_diagnostics_are_source_facing() {
    let project = Project::new("diagnostics");
    project.write(
        "models.sagate",
        "export active = table \"public\" \"users\"\n",
    );
    project.write(
        "missing_file.sagate",
        "import { x } from \"./gone.sagate\"\n",
    );
    let error = project.error("missing_file.sagate");
    assert!(error.contains("missing_file.sagate:1"), "{error}");
    assert!(error.contains("cannot read './gone.sagate'"), "{error}");
    assert!(error.contains("file not found"), "{error}");

    project.write(
        "missing_export.sagate",
        "import { nope } from \"./models.sagate\"\n",
    );
    let error = project.error("missing_export.sagate");
    assert!(error.contains("does not export 'nope'"), "{error}");

    project.write(
        "duplicate.sagate",
        "import { active as a } from \"./models.sagate\"\n\
         import { active as a } from \"./models.sagate\"\n",
    );
    let error = project.error("duplicate.sagate");
    assert!(error.contains("duplicate local name 'a'"), "{error}");
    assert!(error.contains("use 'as'"), "{error}");

    project.write(
        "shadow.sagate",
        "import { active } from \"./models.sagate\"\nactive = 1\n",
    );
    let error = project.error("shadow.sagate");
    assert!(error.contains("duplicate binding 'active'"), "{error}");

    project.write(
        "forward.sagate",
        "q = later & limit 1\nlater : query { id = int } = table \"public\" \"t\"\n",
    );
    let error = project.error("forward.sagate");
    assert!(
        error.contains("unknown variable 'later'") || error.contains("unknown name 'later'"),
        "{error}"
    );

    project.write("unknown_export.sagate", "value = 1\nexport { nothing }\n");
    let error = project.error("unknown_export.sagate");
    assert!(
        error.contains("cannot export unknown name 'nothing'"),
        "{error}"
    );
}

/// A failure inside a dependency names the import chain that reached it.
#[test]
fn dependency_failures_name_their_import_chain() {
    let project = Project::new("chain");
    project.write("broken.sagate", "export bad : int = not_defined\n");
    project.write(
        "middle.sagate",
        "import { bad } from \"./broken.sagate\"\nexport middle = bad\n",
    );
    project.write("top.sagate", "import { middle } from \"./middle.sagate\"\n");
    let error = project.error("top.sagate");
    assert!(error.contains("./middle.sagate"), "{error}");
    assert!(error.contains("./broken.sagate"), "{error}");
    assert!(error.contains("unknown name 'not_defined'"), "{error}");
    // Internal symbols never leak into user-facing diagnostics.
    assert!(!error.contains("$m"), "{error}");
}

/// Source-only compilation rejects imports instead of resolving them against
/// the working directory.
#[test]
fn source_only_apis_reject_imports() {
    let error = crate::compile_source("import { x } from \"./a.sagate\"\n")
        .expect_err("imports need a file path");
    assert!(error.contains("imports require a file path"), "{error}");
    assert!(!error.contains("$"), "{error}");
}

/// Source compilation resolves prelude aliases against the prelude too:
/// overriding `identity` does not re-point `id`.
#[test]
fn source_compilation_keeps_prelude_aliases_resolved() {
    let queries = crate::compile_source(
        "identity : int -> int = value => value + 1\n\
         users : query { id = int } = table \"public\" \"users\"\n\
         q = users & select (row => { a = identity (row.id), b = id (row.id) })\n",
    )
    .expect("compile source");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("q.\"id\" + 1 AS \"a\""), "{sql}");
    assert!(sql.contains("q.\"id\" AS \"b\""), "{sql}");
}

/// Source compilation validates its export list the way the linker validates
/// a file's exports: every published name exists and appears once.
#[test]
fn source_exports_are_validated() {
    let error =
        crate::compile_source("value = 1\nexport { nothing }\n").expect_err("unknown export");
    assert!(
        error.contains("<source>:2: cannot export unknown name 'nothing'"),
        "{error}"
    );

    let error = crate::compile_source("value = 1\nexport { value, value }\n")
        .expect_err("duplicate public name");
    assert!(error.contains("duplicate export 'value'"), "{error}");

    let error =
        crate::compile_source("export value = 1\nexport { value }\n").expect_err("published twice");
    assert!(error.contains("duplicate export 'value'"), "{error}");
}

/// Import paths must name an exact `.sagate` file, matching the docs.
#[test]
fn import_paths_must_name_a_sagate_file() {
    let project = Project::new("import-extension");
    project.write("models.sagate", "export shown : int = 2\n");
    project.write("main.sagate", "import { shown } from \"./models\"\n");
    let error = project.error("main.sagate");
    assert!(error.contains("main.sagate:1"), "{error}");
    assert!(error.contains("must name a .sagate file"), "{error}");
}

/// Exports govern access; `export { name as public }` publishes a local
/// binding under a different name, and the original stays private.
#[test]
fn export_lists_publish_under_new_names() {
    let project = Project::new("export-list");
    project.write(
        "models.sagate",
        "normalize : string -> string = sql \"LOWER($1)\"\n\
         shout : string -> string = sql \"UPPER($1)\"\n\
         export { normalize, shout as upper_name }\n",
    );
    project.write(
        "report.sagate",
        "import { normalize, upper_name } from \"./models.sagate\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { n = normalize(row.name), u = upper_name(row.name) })\n",
    );
    let queries = project.compile("report.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("LOWER(q.\"name\") AS \"n\""), "{sql}");
    assert!(sql.contains("UPPER(q.\"name\") AS \"u\""), "{sql}");
    // The original spelling stays private.
    let project_private = Project::new("export-list-private");
    project_private.write(
        "models.sagate",
        "shout : string -> string = sql \"UPPER($1)\"\nexport { shout as upper_name }\n",
    );
    project_private.write(
        "report.sagate",
        "import { shout } from \"./models.sagate\"\n",
    );
    let error = project_private.error("report.sagate");
    assert!(error.contains("does not export 'shout'"), "{error}");
}

/// `export name = ...` publishes one name; an attempt to import the local
/// spelling of a renamed export fails.
#[test]
fn define_and_export_publishes_only_the_written_name() {
    let project = Project::new("define-export");
    project.write(
        "models.sagate",
        "export shout : string -> string = sql \"UPPER($1)\"\n",
    );
    project.write(
        "ok.sagate",
        "import { shout } from \"./models.sagate\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { u = shout(row.name) })\n",
    );
    let queries = project.compile("ok.sagate").expect("compile");
    assert!(query(&queries, "q")
        .sql
        .contains("UPPER(q.\"name\") AS \"u\""));
}

/// A file that is only reached through `..` or a symbolic link keeps one
/// identity, so its queries are never duplicated.
#[test]
fn canonical_paths_deduplicate_imports() {
    let project = Project::new("canonical");
    project.write(
        "shared/text.sagate",
        "export normalize : string -> string = sql \"LOWER($1)\"\n",
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        project.root.join("shared"),
        project.root.join("shared_link"),
    )
    .expect("create symlink");
    project.write(
        "via_link.sagate",
        "import { normalize } from \"./shared_link/text.sagate\"\n\
         export upper_via_link : string -> string = sql \"UPPER($1)\"\n",
    );
    project.write(
        "via_real.sagate",
        "import { normalize } from \"./shared/text.sagate\"\n\
         import { upper_via_link } from \"./via_link.sagate\"\n\
         export both : string -> string = name => upper_via_link(normalize(name))\n",
    );
    project.write(
        "main.sagate",
        "import { both } from \"./via_real.sagate\"\n\
         users : query { name = string } = table \"public\" \"users\"\n\
         q = users & select (row => { n = both(row.name) })\n",
    );
    let queries = project.compile("main.sagate").expect("compile");
    let sql = &query(&queries, "q").sql;
    assert!(sql.contains("UPPER(LOWER(q.\"name\"))"), "{sql}");
}

/// An unimported name from another file is never visible, even when that
/// file is loaded for another import.
#[test]
fn unimported_names_stay_invisible() {
    let project = Project::new("visibility");
    project.write("other.sagate", "hidden : int = 1\nexport shown : int = 2\n");
    project.write(
        "main.sagate",
        "import { shown } from \"./other.sagate\"\n\
         users : query { id = int } = table \"public\" \"users\"\n\
         q = users & select (row => { id = row.id + hidden + shown })\n",
    );
    let error = project.error("main.sagate");
    assert!(error.contains("hidden"), "{error}");
    assert!(
        error.contains("unknown variable 'hidden'") || error.contains("unknown name 'hidden'"),
        "{error}"
    );
}

/// Every loaded definition must type-check, including private and unused ones.
#[test]
fn private_and_unused_definitions_still_type_check() {
    let project = Project::new("unused-check");
    project.write(
        "models.sagate",
        "export shown : int = 1\n\
         broken : int = sql \"LOWER($1)\"\n",
    );
    project.write(
        "main.sagate",
        "import { shown } from \"./models.sagate\"\n\
         users : query { id = int } = table \"public\" \"users\"\n\
         q = users & limit shown\n",
    );
    let error = project.error("main.sagate");
    assert!(
        error.contains("SQL template requires a function type signature"),
        "{error}"
    );
}

/// The `--dialect` selection applies to module compilation.
#[test]
fn module_compilation_honors_the_dialect() {
    let project = Project::new("dialect");
    project.write(
        "main.sagate",
        "users : query { id = int } = table \"public\" \"users\"\n\
         q = users & limit 3\n",
    );
    let ansi = project
        .compile_dialect("main.sagate", "ansi")
        .expect("ansi");
    let postgres = project
        .compile_dialect("main.sagate", "postgres")
        .expect("postgres");
    assert_eq!(ansi.len(), postgres.len());
    assert_eq!(query(&ansi, "q").sql, query(&postgres, "q").sql);

    let error = project
        .compile_dialect("main.sagate", "not-a-dialect")
        .expect_err("unknown dialect");
    assert!(error.contains("unknown SQL dialect"), "{error}");
}

/// The repository's own examples compile through the file API.
#[test]
fn repository_examples_compile_as_files() {
    for example in [
        "examples/users.sagate",
        "examples/report.sagate",
        "examples/modules/report.sagate",
    ] {
        let path = project_root().join(example);
        let linked = link_file(&path).unwrap_or_else(|error| panic!("{example}: {error}"));
        compile_linked_with_dialect(&linked, "ansi")
            .unwrap_or_else(|error| panic!("{example}: {error}"));
    }
}
