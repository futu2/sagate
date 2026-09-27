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
Comparison operators use the same function application form; the SQL backend
recognizes the built-in comparison functions when they occur in a `where`
predicate. Arithmetic, comparison, and boolean operator sections are exported
by the prelude, so they can be passed or partially applied like any other
function. Arithmetic overloads are explicit: `+` and `-` each have an `int`
case and a `float` case. There is no numeric type variable or implicit numeric
coercion, so mixed expressions such as `1 + 2.0` are rejected.

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

Public functions and operators are defined in `prelude.sagate`. The
double-underscore primitives are declared there with their signatures, and
those declarations are the type-level source of truth; the Rust compiler
implements the primitives for SQL generation and gives the relational ones
their row typing. User bindings can override prelude definitions; the
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
and [`examples/report.sagate`](examples/report.sagate).

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
`-`.

## Rust modules

- `src/lang/`: source AST, lexer/parser, row types, and type checking.
- `src/sql.rs`: typed relational compilation to quoted SQL subqueries.
- `src/main.rs`: CLI entry point.
- `flake.nix`: reproducible Nix development shell and package.

Rust callers can select a dialect with `compile_with_dialect`:

```rust
let queries = sagate::compile_with_dialect(&program, "postgres")?;
```
