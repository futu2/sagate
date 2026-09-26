# Kinded HM With Extended Rows

## Goal

The type core is Hindley-Milner plus records whose rows can be open. A row is
an ordered sequence of bindings followed by either `Empty` or a row variable:

```text
Row ::= Empty | { label : Type | Row } | r
Type ::= int | string | maybe Type | Type -> Type | { Row } | alpha
```

Rows are scoped: a repeated label may be introduced while building a row, and
the rightmost binding is the live one. `merge old new` makes this rule
explicit. It keeps old field positions, replaces a colliding old value with
the new value, and appends labels that occur only in `new`.

```text
merge { id:int, left:string } { id:string, right:int }
  = { id:string, left:string, right:int }
```

There is no implicit subtyping relation. A row variable in a polymorphic
function absorbs surplus fields during unification; a closed record is only
accepted where its exact fields unify.

## Kinded Fω-shaped row core

The term language uses Hindley-Milner inference. Its row fragment uses the
first-order, terminating part of an Fω design: type-level row applications are
explicit terms, and every term has a kind.

```text
Kind ::= Type | Row | KeyMap | ValueMap
```

`query` and `row` are type constructors from `Row` to `Type`. `query r` is a
relation value, while `row r` is one record value. Rows also carry an extent:
`closed {a:int}` contains exactly `a`, while `open {a:int | r}` may contain
additional fields in its opaque tail.

The kind rules are explicit:

```text
Γ ⊢ r : Row                 Γ ⊢ r : Row
───────────────             ───────────────
Γ ⊢ row r : Type             Γ ⊢ query r : Type
```

Therefore an arrow such as `row r -> row s` is a function between values of
kind `Type`. A bare `r -> s` uses ordinary HM type variables of kind `Type`;
it does not silently turn them into rows.

```text
RowTerm ::= closed { label : Type, ... }
         | open { label : Type, ... | r }
         | r
         | merge RowTerm RowTerm
         | mapkey KeyMapTerm RowTerm
         | mapvalue ValueMapTerm RowTerm
```

The formation rules reject mapper-axis mistakes before unification:

```text
Gamma |- m : KeyMap       Gamma |- r : Row
------------------------------------------- mapkey m r : Row

Gamma |- v : ValueMap     Gamma |- r : Row
------------------------------------------- mapvalue v r : Row
```

The corresponding prelude schemes are:

```text
mapKey   : forall m:KeyMap. forall r:Row.
           keymapper m -> query r -> query (mapkey m r)
mapValue : forall v:ValueMap. forall r:Row.
           valuemapper v -> query r -> query (mapvalue v r)
merge    : forall r1:Row. forall r2:Row.
           query r1 -> query r2 -> query (merge r1 r2)
```

Instantiation freshens value, row, key-mapper, and value-mapper variables in
separate namespaces. Overloads are finite sets of HM schemes; an application
tests each case with a cloned substitution state and commits the unique
matching case.

## Mapping primitives

Key and value mappings are separate axes:

```text
mapKey   : KeyMap -> row r -> row (MapKey KeyMap r)
mapValue : ValueMap -> row r -> row (MapValue ValueMap r)
```

The witnesses are closed data, not arbitrary type-level functions:

```text
KeyMap   = Id | Prefix string | Suffix string | Snake | Kebab | Camel
         | Replace string string | Compose KeyMap KeyMap
ValueMap = Id | maybe | List
```

For a closed row these reduce field by field. A non-injective key map can
produce the same label more than once; `live` resolves that collision to the
newer binding, just as `merge` does. Mapping never changes field value types on
the key axis and never changes labels on the value axis.

For an open row, the hidden tail cannot be inspected safely. The result is
therefore retained as `MapKey f r` or `MapValue g r`, rather than guessing its
labels. The inferencer carries that expression in the row and can normalize it
after a later substitution makes the row concrete.

## HM inference

`infer` implements the standard rules for variables, literals, lambda,
application, and let-generalization. Records add these rules:

```text
select l : { l : alpha | r } -> alpha
extend l e r : merge r { l : type(e) }
merge x y   : merge (row(x)) (row(y))
mapKey f r  : MapKey f (row(r))
mapValue g r: MapValue g (row(r))
```

The executable surface uses the same operations as ordinary functions. Function
application is curried and left-associative, so `f a b` means `(f a) b`. The
forward application operator is an ordinary infix function:

```text
x & f       = f x
x & f a b   = f a b x
```

`&` and `$` are ordinary prelude functions. `&` applies its right operand to
its left operand, while `$` applies its left operand to its right operand.
`>>>` and `<<<` are also ordinary prelude functions.

Infix operators use an Agda-style section name, and the section name is the
function's ordinary identifier:

```text
_+_ : int -> int -> int = x => y => x
left = _+_ 1 2
right = 1 + 2
```

The numeric operators are declared as finite overloads rather than as a
numeric type class:

```text
_+_ : int -> int -> int
_+_ : float -> float -> float
_-_ : int -> int -> int
_-_ : float -> float -> float
```

The two application forms produce the same nested `Apply` expression:
`_+_ a b` and `a + b` both apply `+` to `a` and then to `b`. The prelude's
comparison functions call SQL comparison primitives when used with `where`.

The relational prelude therefore has these curried schemes (with the row
operation shown explicitly):

```text
table    : string -> string -> query r
where    : (row r -> bool) -> query r -> query r
select   : (row r -> row s) -> query r -> query s
mapKey   : keymapper m -> query r -> query (mapkey m r)
mapValue : valuemapper v -> query r -> query (mapvalue v r)
merge    : query r -> query s -> query (merge r s)
```

The SQL step layer extends the same model with grouping and join mode
functions:

```text
agg       : (row r -> row s) -> query r -> query s
inner      : query r -> (row l -> row r -> bool) -> query l -> query c
left       : query r -> (row l -> row r -> bool) -> query l -> query c
right      : query r -> (row l -> row r -> bool) -> query l -> query c
full       : query r -> (row l -> row r -> bool) -> query l -> query c
```

Join rows use `merge right left`, so fields from the left input overwrite
duplicate fields from the right input. The selected outer join mode controls
which side becomes nullable.

Aggregate projections use `group`, `count`, `sum`, `avg`, `min`, and `max`.
Scalar and schema types include `int`, `float`, `string`, `bool`, `date`, and
`timestamp`.

`let` bindings extend the HM environment and can be reused by later
expressions. The Rust implementation keeps concrete relation rows alongside
these function types so SQL field references can be checked before lowering.

In the source type language, `query r` is a relation-row variable rather than
an empty record. Row variables are ordinary type variables whose kind is fixed
by their use in `query r`: the same variable can appear directly as a function
argument or result. A predicate is `row r -> bool`, and a projection is
`row r -> row s`; `row` lifts a row-kind term into a record value type. They are
not special marker types. Concrete SQL expressions
refine those rows at the call site.

### Explicit overloads

Numeric operators do not use an unconstrained type variable. `+` and `-` are
finite overload sets with separate `int -> int -> int` and
`float -> float -> float` cases. This keeps the ordinary type language
decidable and makes mixed numeric expressions fail without an implicit
coercion rule.

User code can define the same name more than once with concrete signatures:

```sagate
choose : int -> int = x => x
choose : float -> float = x => x
```

The checker resolves an overload at application time from the argument type.
The cases are represented explicitly in the expression and type trees, rather
than being collapsed into a wildcard `Any` type. Lowercase variables are
scoped to one signature, so `a -> b -> a` and `a -> a -> a` have different
constraints.

Unification matches shared visible labels and binds open tails to fields found
only on the other side. If an operation still contains an opaque row
expression, equality is recorded in the state's deferred constraint list.
This is the deliberate boundary of the design: inference remains HM for the
ordinary language, while non-local row operations are represented explicitly
until their inputs are available.

## Rust implementation

- `src/lang.rs`: the source AST, parser, row operations, and type checking.
- `src/sql.rs`: SQL compilation over typed relation subqueries.
- `src/main.rs`: the command-line compiler.
- `examples/users.sagate`: a complete source program.
