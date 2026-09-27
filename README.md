# Sagate

Sagate is a small Rust language for typed relational queries. It has an
extensible row type model, `mapKey` and `mapValue` row operations, right-biased
`merge`, a handwritten parser, and a SQL compiler. SQL is assembled as an AST
with [`sql-glot-rust`](https://github.com/protegrity/sql-glot-rust) and then
rendered for the requested dialect. The expression core is
purely functional: a query chain is ordinary function application, and the
relational operations are prelude functions.

The checker uses HM inference with a kinded first-order Fω row core. `Type`,
`Row`, `KeyMap`, and `ValueMap` are separate namespaces; row expressions retain
`merge`, `mapkey`, and `mapvalue` until they can normalize. Rows also retain
their extent (`closed` annotations versus open table tails), so a closed
annotation cannot silently accept extra fields. See
[`type-system-design.md`](type-system-design.md) for the typing rules.

## Language

```sagate
users : query { id = int, displayName = string, active = bool } =
  table "public" "users"

active_users = users
  & where (.active == true)
  & mapKey snake
  & select { id = .id, name = .display_name }
```

`&` is a prelude function for forward application. Its left operand is the
value and its right operand is the function:

```sagate
active_users = users & where (.active == true)
```

The prelude defines both `&` and `$` as ordinary operator functions:

```sagate
users & where (.active == true)
where (.active == true) $ users
```

`$` applies a function to a value. The prelude also defines `>>>` and `<<<` as composition operators:
`f >>> g` applies `f` then `g`, while `f <<< g` applies `g` then `f`.

Bindings and local let expressions are ordinary functional values. A binding
can be reused by another relation:

```sagate
active = users & where (.active == true)
names = active
  & mapKey snake
  & select { id = .id, name = .display_name }
```

The core AST has `Var`, `Lambda`, `Apply`, and `Let` nodes. `x => ...` is the
lambda syntax; applied lambdas are beta-reduced when their result is a SQL
relation, while arbitrary value-producing lambdas remain language-level
Lambda expressions use the `x => ...` syntax.

Infix operators are ordinary function names written between underscores, as
in Agda. The section name and infix spelling use the same function:

```sagate
_+_ : int -> int -> int = x => y => x

left = _+_ 1 2
right = 1 + 2
```

Both applications elaborate to applying `+` to `1` and then to `2`.
Comparison operators use the same function application form. The prelude gives
arithmetic, comparison, and boolean operators SQL expression templates, so they
can be passed or partially applied like any other function and still lower in a
`where` predicate. Arithmetic overloads are explicit: `+` and `-` each have an
`int` case and a `float` case. There is no numeric type variable or implicit
numeric coercion, so mixed expressions such as `1 + 2.0` are rejected.

Named functions can have a finite overload set. Give each case its own concrete
function signature; application selects the case whose argument types match:

```sagate
clamp : int -> int = value => value
clamp : float -> float = value => value

whole = clamp 1
fractional = clamp 1.0
```

Type variables in one signature are distinct and scoped to that signature. For
example, `a -> b -> a` accepts arguments of different types and returns the
first one. A repeated variable such as `a -> a -> a` requires both arguments to
have the same type.

`table` is also a prelude function:

```text
table : string -> string -> query r
```

The arguments are `schemaName` and `tableName`. The result has an open row
variable `r`. Give the row its shape at the binding
(`users : query { id = int, name = string } = table "public" "users"`), with
an inline annotation (`table "public" "users" : query { id = int }`), or leave
it open and let later steps refine the fields they touch. `query r -> query r`
preserves one row variable, while `query r -> query s` explicitly describes a
row transforming operation.

Predicates and projections are ordinary functions: a predicate has type
`row r -> bool`, and a projection has type `row r -> row s`. Field shorthand
is syntax sugar for these functions: `(.active == true)` means
`row => row.active == true`, and `{id = .id}` means
`row => {id: row.id}`. In a two-row predicate, `that` names the second row,
so `(.user_id == that.owner_id)` means
`row_left => row_right => row_left.user_id == row_right.owner_id`.
The `row`
constructor lifts a row-kind term into a record value type; `query` lifts it into
a relation type.
Both `table("public", "users")` and the curried spelling
`table "public" "users"` construct the same application.

Top-level relation bindings are ordinary bindings. The `query` word is only a
relation type constructor used in annotations; `from` is not a source form.
Definitions end at a new line; semicolons are optional compatibility syntax.

`mapKey snake` changes the typed row labels and emits SQL aliases. The prelude
also provides `prefix "text"` and `suffix "text"` key mappers. `mapValue`
changes the value types while retaining labels. It does not change the SQL
expressions, although the compiler may add a projection wrapper:

```sagate
nullable_users = users
  & mapValue maybe
  & select { id = .id, name = .displayName }
```

The language has scalar types (`int`, `float`, `string`, `bool`, `date`, and
`timestamp`). The prelude defines temporal constructors and SQL grouping
functions. `agg` groups and computes columns. Both inputs need to be defined
as relations, for example:

```sagate
users : query { id = int, name = string } = table "public" "users"
orders : query { user_id = int, total = float } = table "public" "orders"

totals = orders
  & agg { user_id = group .user_id, total = sum .total, rows = count }

report = totals
  & inner users (left => right => left.user_id == right.id)
  & select { user_id = .user_id, name = .name, total = .total, rows = .rows }
```

`inner`, `left`, `right`, and `full` are the join mode functions. Each takes the
right query and a two-row predicate; `&` supplies the left query. The nullable
side of an outer join is reflected in the result row type.

When both inputs
contain a field, the left input wins, equivalent to `merge right left`.
Join predicates can use the two-row shorthand described above.

The language has two collection forms. A bracketed list `[a1, a2, a3]` is an
ordered list of values, and a braced row `{x = v, y = w}` is a row — at both
the value level (`{ x = 1, label = "origin" }`) and the type level
(`query { id = int, name = string }`); braces always pair fields with `=`.
Field values in a row literal are ordinary expressions; when they contain
`.field` markers the literal becomes a row function — that is how `select`
and `agg` projections are written, and it lets projections compute new
columns:

```sagate
point : row { x = int, label = string } = { x = 1, label = "origin" }

totals = orders
  & agg { user_id = group .user_id, total = sum .total }
```

`order` sorts a query. Its key function maps each row to a list of
`asc`/`desc` tagged key values, and a bracketed list argument denotes that
function. Keys are orderable scalars (`int`, `float`, `string`, `bool`,
`date`, `timestamp`, and their `maybe` variants). `limit` takes an integer
literal count (a named constant binding also works):

```sagate
users : query { id = int, last_name = string, age = int } = table "public" "users"

oldest = users
  & order [desc .age, asc .last_name]
  & limit 10

by_name = users & order [asc .last_name]
by_age = users & order [desc .age] & limit 10
```

Dot access is whitespace-sensitive: `row.name` (no space) reads the field on
`row`, while a spaced `.name` after a function is the implicit row-function
argument — that is why `asc .last_name` works as a sort key. One caveat:
inside an explicit lambda, `asc row.last_name` still parses `row.last_name`
as access on the application, so spell it `asc(row.last_name)` there.

The compiler lowers every pipeline step into a CTE and emits one flat
`WITH` chain, then runs sqlglot-rust's optimization passes (constant folding,
boolean simplification, limit-aware predicate pushdown) over the result:

```sql
WITH "q0" AS (SELECT * FROM "public"."users"),
     "q1" AS (SELECT * FROM "q0" AS q WHERE q."active" = TRUE)
SELECT * FROM "q1" AS q ORDER BY q."age" DESC
```

Because each step keeps its position in the chain, `order` sorts at the step
where it appears: place it last, or directly before `limit`, since an outer
projection is free to reorder rows.

Public functions and operators are defined in `prelude.sagate`. Scalar SQL
functions use `sql` expression templates with positional placeholders; the
compiler parses each template as a SQL expression and substitutes typed
arguments before rendering the requested dialect:

```sagate
lower : string -> string = sql "LOWER($1)"
between : int -> int -> int -> bool = sql "$1 BETWEEN $2 AND $3"
```

The prelude defines arithmetic, comparison, boolean, and common string
functions this way, along with the SQL calls for `count`, `sum`, `avg`, `min`,
and `max`. For example, `lower(row.name)` can be used inside a projection.
The Rust compiler retains the structural relational primitives (`table`,
`where`, joins, and query transformations), which build the query AST and
provide row typing. User bindings can override prelude definitions; the
previous definition remains available within the overriding binding.
Aggregate constructors can also be aliased or overridden through bindings.
Predicates in `where` and joins accept full scalar expressions — fields,
literals, comparisons, `&&`/`||`, and arithmetic.

`merge older newer` compiles to a cross join. If both rows contain a label,
the newer row supplies the selected SQL expression and type:

```sagate
user_names : query { id = int, name = string } = table "public" "user_names"
combined = merge users user_names
  & select { id = .id, name = .name }
```

Complete examples are in [`examples/users.sagate`](examples/users.sagate)
and [`examples/report.sagate`](examples/report.sagate); a multi-file project
is in [`examples/modules`](examples/modules).

## Modules

Each `.sagate` file is a module. Bindings are private unless you export them,
and another file imports selected names through a relative path:

```sagate
# models/users.sagate
users : query { id = int, name = string, active = bool } =
  table "public" "users"

export active_users = users & where (.active == true)

export normalize : string -> string = sql "LOWER($1)"
```

```sagate
# report.sagate
import { active_users, normalize as normalize_name } from "./models/users.sagate"

report = active_users
  & select (row => { id = row.id, name = normalize_name(row.name) })
```

`import` and `export` are contextual keywords: they open a declaration only in
their full syntactic form, so a binding such as `export = 1` still parses as an
ordinary definition. Imports must appear before the first binding, use string
literal paths that start with `./` or `../`, and name an exact `.sagate` file.
Paths resolve against the importing file's directory, so the shell's working
directory never affects resolution; `..` segments and symbolic links are
canonicalized so a shared dependency loads once.

Two export forms are available. `export name : type = ...` defines and exports
a binding; `export { name, other as public_name }` publishes existing
definitions, in any order relative to them:

```sagate
normalize : string -> string = sql "LOWER($1)"
page_size : int = 20
export { normalize, page_size as default_page_size }
```

An exported name can be re-exported through another module's export list, and
overloaded names and operators export as a whole:

```sagate
# models/users.sagate publishes a re-export.
import { active_users } from "./models/users.sagate"
export { active_users as users }

# Operators keep their section spelling.
import { _+_ as add } from "./arithmetic.sagate"
```

Every file has its own top-level scope: two modules may both define `users` or
override `+` without affecting each other, and an imported function keeps
resolving to its own module's helpers regardless of the importer's definitions.
Names must be defined before use; a forward reference to a later local binding
is an error. Import cycles fail with the full cycle in the message.

Compilation reads source files only — importing a query never runs it or
touches the database. SQL output follows the entry file: locally defined
queries compile in definition order under their written names, while queries in
dependencies are available for those pipelines but never emitted on their own.
Compiling `models/users.sagate` directly therefore emits both `users` and
`active_users`.

## Development workflow

The flake supplies Rust, Cargo, and rustfmt, so the host does not need a global
Rust installation:

```sh
nix develop
cargo test
cargo run -- examples/users.sagate

# Render another SQL dialect (sql-glot-rust supports aliases such as
# postgres, mysql, sqlite, duckdb, bigquery, and snowflake).
cargo run -- --dialect postgres examples/users.sagate
```

Or compile and run it directly:

```sh
nix run . -- examples/users.sagate
```

The CLI reads a path argument, or stdin when no path is supplied or when passed
`-`. A path is compiled through the module loader, so its relative imports are
resolved from the file's directory; stdin compiles one anonymous module and
rejects imports.

## Rust API

The crate exposes source- and file-oriented compilation functions. The parser,
checker, and SQL lowering modules remain internal implementation details:

```rust
// One anonymous module. Imports are rejected: there is no path to resolve
// them against.
let queries = sagate::compile_source_with_dialect(source, "postgres")?;

// A file plus its relative imports. Only the entry file's local queries
// become standalone SQL.
let queries = sagate::compile_file_with_dialect("report.sagate", "postgres")?;

for query in queries {
    println!("{}: {}", query.name, query.sql);
}
```

`compile_source` and `compile_file` use the ANSI dialect. The command-line
binary is in `src/main.rs`; `flake.nix` provides the reproducible development
shell and package.
