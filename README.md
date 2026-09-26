# Sagate

Sagate is a small Rust language for typed relational queries. It has an
extensible row type model, `mapKey` and `mapValue` row operations, right-biased
`merge`, a handwritten parser, and a SQL compiler. The expression core is
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
users : query { id: int, displayName: string, active: bool } =
  table "public" "users"

active_users = users
  & where (.active == true)
  & mapKey snake
  & select { id: .id, name: .display_name }
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
  & select { id: .id, name: .display_name }
```

The core AST has `Var`, `Lambda`, `Apply`, and `Let` nodes. `x => ...` is the
lambda syntax; applied lambdas are beta-reduced when their result is a SQL
relation, while arbitrary value-producing lambdas remain language-level
values. `fn x => ...` remains accepted as compatibility syntax.

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

Definitions can declare a finite overload set with `def`. Each case needs its
own concrete function signature; application selects the case whose argument
types match:

```sagate
def clamp : int -> int = value => value
def clamp : float -> float = value => value

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
variable `r`; the annotation on `users` gives the compiler the visible table
shape. `query r -> query r` preserves one row variable, while `query r ->
query s` explicitly describes a row transforming operation.

Predicates and projections are ordinary functions: a predicate has type
`row r -> bool`, and a projection has type `row r -> row s`. The `row`
constructor lifts a row-kind term into a record value type; `query` lifts it into
a relation type.
Both `table("public", "users")` and the curried spelling
`table "public" "users"` construct the same application.

Top-level relation bindings are ordinary bindings. The `query` word is only a
relation type constructor used in annotations; `from` is not a source form.
Definitions end at a new line; semicolons are optional compatibility syntax.

`mapKey snake` changes the typed row labels and emits SQL aliases. The prelude
also provides `prefix "text"` and `suffix "text"` key mappers. `mapValue`
changes the value types while retaining labels; it is type-only at SQL runtime:

```sagate
nullable_users = users
  & mapValue maybe
  & select { id: .id, name: .displayName }
```

The language has scalar types (`int`, `float`, `string`, `bool`, `date`, and
`timestamp`). The prelude defines temporal constructors and SQL grouping
functions. `agg` groups and computes columns:

```sagate
totals = orders
  & agg { user_id: group .user_id, total: sum .total, rows: count }
```

`join` is a prelude function. It takes a configuration function that names the
join mode, right query, and two-row predicate, while `&` supplies the left query:

```sagate
report = totals
  & join (inner users (left => right => left.user_id == right.id))
```

Use `left`, `right`, or `full` instead of `inner` for the other join modes.
Their nullable side is reflected in the result row type. When both inputs
contain a field, the left input wins, equivalent to `merge right left`.
Join predicates currently compare one field from each input.

Public functions and operators are defined in `prelude.sagate`.
The Rust compiler recognizes their double-underscore primitives for typing
and SQL generation. User bindings can override prelude definitions; the
previous definition remains available within the overriding binding.
Aggregate constructors can also be aliased or overridden through bindings.

`merge(older, newer)` compiles to a cross join. If both rows contain a label,
the newer row supplies the selected SQL expression and type:

```sagate
combined = merge users users
  & select { id: .id, name: .displayName }
```

Complete examples are in [`examples/users.sagate`](examples/users.sagate)
and [`examples/report.sagate`](examples/report.sagate).

## NixOS workflow

The flake supplies Rust, Cargo, and rustfmt, so the host does not need a global
Rust installation:

```sh
nix develop
cargo test
cargo run -- examples/users.sagate
```

Or compile and run it directly:

```sh
nix run . -- examples/users.sagate
```

The CLI reads a path argument or stdin when passed `-`.

## Rust modules

- `src/lang.rs`: source AST, lexer/parser, row types, and type checking.
- `src/sql.rs`: typed relational compilation to quoted SQL subqueries.
- `src/main.rs`: CLI entry point.
- `flake.nix`: reproducible Nix development shell and package.
