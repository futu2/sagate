# Sagate module system

Status: implemented. Each `.sagate` file is a module; `import` and `export`
are supported, and the file-oriented APIs (`compile_file`,
`compile_file_with_dialect`) load relative imports. The deferred items listed
under "Scope of the first version" remain out of scope.

Each `.sagate` file is a module. Bindings are private unless explicitly
exported, and another file imports selected names through a relative path.
Imports are resolved during compilation; they introduce no runtime loading or
database execution.

## Example

`models/users.sagate`:

```sagate
# A private query that an exported query can use.
users : query { id = int, name = string, active = bool } =
  table "public" "users"

export active_users = users & where (.active == true)

export normalize : string -> string = sql "LOWER($1)"
```

`report.sagate`:

```sagate
import { active_users, normalize as normalize_name } from "./models/users.sagate"

report = active_users
  & select (row => { id = row.id, name = normalize_name(row.name) })
```

Compiling `report.sagate` emits one query named `report`. The compiler retains
the imported definitions and their private dependencies to generate its SQL.
An attempt to import `users` fails because that name is private.

## Syntax and visibility

Use named imports and permit aliases:

```sagate
import { users } from "./models.sagate"
import { orders as recent_orders, customers } from "./sales.sagate"
```

Put imports before the first binding or export declaration. Import lists can
span lines and have a trailing comma. Declarations end at a newline or a
semicolon, consistent with existing definitions. Imports are allowed only at
the top level and their paths must be string literals.

Two export forms serve different purposes:

```sagate
# Define and export a binding.
export page_size : int = 20

# Publish existing bindings, optionally under different names.
normalize : string -> string = sql "LOWER($1)"
export { normalize, page_size as default_page_size }
```

`export name ... = ...` is shorthand for an ordinary definition plus
`export { name }`. Export lists are checked after all definitions in the file,
so they may appear before or after the definitions they publish. The same
public name cannot appear twice, including through both export forms. Two
different public names may refer to the same binding.

An export must refer to a local definition or an explicit import. Prelude
names require a local alias before they can be exported. Exported values may
be queries, scalar values, functions, SQL templates, mapper values, or whole
overload sets. Exporting does not require a type annotation beyond the
language's existing requirements; annotating public functions and table rows
is recommended to make a module's contract clear.

An imported name can be exported again:

```sagate
# models.sagate: a public entry point for several model files.
import { active_users } from "./models/users.sagate"
export { active_users as users }
```

This is sufficient for re-exports in the first version. It preserves the
original binding's identity and does not copy or re-infer its declaration.

`import` and `export` should be contextual keywords: recognize declarations
by their full syntactic form, preserving existing bindings such as
`export = 1`. `as` and `from` are special only within module declarations.

## Names, overloads, and lexical scope

Every file has its own top-level scope. Two files may both define `users` or
`normalize` without a conflict. Inside a function, free names refer to the
module where the function was defined. An importer cannot change an exported
function's meaning by defining a helper with the same name.

Within a file, apply these rules:

| Situation | Rule |
| --- | --- |
| Two imports introduce the same local name | Error; use `as` to distinguish them. |
| A local definition has the same name as an import | Error; rename the import or definition. |
| An import shadows a prelude name | Allowed in this file. |
| A local definition shadows a prelude name | Preserve Sagate's existing override behavior. |
| A lambda parameter or local `let` shadows a top-level name | Allowed in its lexical scope. |
| A reference uses a later local definition | Error; retain definition order. |
| A reference uses an unimported name from another file | Error, even if that file is loaded elsewhere. |

For a prelude override, the overriding definition can still reference the
previous prelude definition in its own body, as Sagate supports today. Later
definitions see the override. The shared prelude and other modules retain
their own resolved references. This requires resolving prelude bindings once
and resolving each module independently.

Repeated annotated definitions keep the existing overload rules. Exporting
an overloaded name exports its entire set of cases; visibility is attached
to the name, not individual cases. Imported overload sets remain intact and
cannot be extended with local cases. For clarity, publish overloaded names
with an export list after their definitions.

Operators use their existing section spelling in import and export lists:

```sagate
import { _+_ as add } from "./arithmetic.sagate"
```

An operator imported without an alias is available with its existing infix
syntax and precedence. Importing an operator does not change parsing rules.
Names beginning with `__` remain reserved for backend primitives and cannot
be introduced or exported through module declarations.

## File resolution

In the first version, an import path must start with `./` or `../` and name
an exact `.sagate` file. Resolve it against the importing file's directory.
Require the extension; do not search for alternative extensions or directory
entry files. Package names, absolute import paths, and network URLs are
outside this version. The CLI entry path may still be absolute.

For example, `models/users.sagate` importing `../shared/text.sagate` resolves
to `shared/text.sagate` in the same project, regardless of the shell's working
directory.

Canonicalize file paths before assigning module identities. This deduplicates
imports through `..` segments or symbolic links. Resolve nested imports from
the canonical file's parent directory, giving a module the same dependencies
regardless of which symbolic link reached it. Retain the written import path
and source location for error messages.

Load each module once per compilation, following imports in source order.
Dependency modules precede their importers; a shared dependency in a diamond
is loaded and linked once. There is no persistent cache in the first version.

Reject import cycles, including cycles formed only through re-exports. Track
the active loading stack and report the complete cycle:

```text
report.sagate:1: import cycle:
  report.sagate -> models.sagate -> report.sagate
```

The loader only reads source files. Importing a query never runs it or
materializes a database table.

## Types and SQL output

Imports preserve the defining binding's type, annotation, overload cases,
and expression body. Row and mapper polymorphism follow the same rules as
local bindings. An import alias must not introduce a fresh declaration with
weaker types or lose the annotation on a SQL template.

For the first implementation, resolve and link the complete dependency graph
before running the existing checker. This allows the checker and SQL backend
to keep inspecting function bodies for relational operations. A public-name
table controls visibility; private definitions remain available internally.
All loaded definitions must type-check, including private and unused ones.

Preserve signature-local type-variable scopes across files. Numeric variable
IDs from two parser instances must not accidentally identify the same type,
row, or mapper variable. Any freshening or relocation must preserve repeated
variables within a signature and their kinds. Modules do not add new type
constructors, implicit coercions, or separate compilation in this version.

SQL output follows the entry file:

| Binding | Emit a standalone SQL query? |
| --- | --- |
| Query defined locally in the entry file | Yes, in definition order. |
| Query defined in a dependency | No; available when compiling entry queries. |
| Query merely imported or re-exported by the entry file | No. |
| Entry binding `report = imported_query` | Yes, named `report`. |
| Scalar, function, mapper, or other non-query binding | No. |

Exports govern access from other files. Entry-file query definitions govern
SQL output. This preserves the behavior of existing single-file programs:
their locally defined queries still compile without adding `export`.

Compiling `models/users.sagate` directly therefore emits both of its local
queries, `users` and `active_users`, even though only the latter is exported.
If selecting specific output queries becomes necessary, add an explicit CLI
selection option independently of visibility.

Module identity and SQL materialization are separate concerns. A dependency
is linked once, but using a query in several outputs may generate its SQL
several times under the existing lowering rules. Imports do not promise CTE
sharing or common-query elimination. Resolve names before SQL lowering;
internal module names must never become database table names or result names.

## Compiler integration

The existing implementation has a useful boundary: `Program` contains a flat
list of bindings, and the checker and SQL compiler consume that list. Add a
module parsing/loading/resolution stage ahead of that core.

```text
entry file
    -> parse files and load dependency graph
    -> resolve private, imported, and exported names
    -> link uniquely named bindings plus entry-output metadata
    -> type-check every binding
    -> lower the selected entry queries to SQL
```

Suggested structures, shown schematically:

```rust
struct ParsedModule {
    imports: Vec<ImportDecl>,
    exports: Vec<ExportDecl>,
    bindings: Vec<Binding>,
    // Preserve source spans on declarations and name references.
}

struct LinkedProgram {
    program: Program,
    outputs: Vec<OutputBinding>, // Internal symbol + source-facing name.
    origins: OriginMap,         // Symbols/spans -> original file and name.
}
```

Each loaded module also needs an export table mapping public names to resolved
binding identities. Aliases point to those identities directly. They should
not become wrapper definitions that obscure SQL templates or overloads.

A small initial implementation can keep string names in the core AST and
assign compiler-only names such as `$m0:users`. These cannot be written as
ordinary source identifiers. Keep original spellings in the origin map;
replacing strings in finished error messages is insufficient for reliable
diagnostics. Dedicated symbol IDs can replace internal strings later.

Resolution must walk expressions with lexical scope information. Rewrite
free variable references while respecting lambda parameters and local `let`
binders. Preserve row labels, accessed field names, and SQL template text.
Reject unresolved names in their own module before flattening, so the flat
environment cannot accidentally expose another module's private binding.
Inlining and substitution must remain capture-avoiding across these scopes.

Keep structural primitive identities stable, since `Intrinsic::from_name`
recognizes their `__...` names. Resolve ordinary prelude functions to their
own stable identities. Audit helpers that recognize public names or operator
spellings so imported aliases and local overrides dispatch through their
resolved definitions rather than accidentally matching a builtin.

The main changes would be:

| Location | Proposed responsibility |
| --- | --- |
| `src/lang/ast.rs` | Module declarations, source metadata, and linked output metadata. |
| `src/lang/parser/grammar.rs` | Parse imports and exports without reading files. |
| `src/lang/parser/prelude.rs` | Separate raw module parsing from prelude assembly; resolve prelude references once. |
| New `src/lang/modules.rs` | Load files, detect cycles, enforce visibility, resolve names, and link bindings. |
| `src/lang/checker/validation.rs` | Check linked bindings and retain their source origins in errors. |
| `src/sql/compiler/entry.rs` | Check the full program, then lower only selected entry bindings. |
| `src/lib.rs` and `src/main.rs` | Add file compilation APIs and route path input through them. |

Filter outputs before SQL generation, rather than generating all dependency
queries and discarding their SQL afterward. Keep every linked definition in
the checker and lowering environments so private dependencies remain usable.

## Public API and CLI

Add file-oriented counterparts to the existing source APIs:

```rust
pub fn compile_file(path: impl AsRef<Path>)
    -> Result<Vec<CompiledQuery>, String>;

pub fn compile_file_with_dialect(path: impl AsRef<Path>, dialect: &str)
    -> Result<Vec<CompiledQuery>, String>;
```

`sagate report.sagate` and `sagate --dialect postgres report.sagate` use these
functions. Preserve the existing output format and original entry binding
names in `CompiledQuery.name`.

`compile_source`, `compile_source_with_dialect`, and stdin compile a single
anonymous module. They accept export declarations but reject imports with a
message explaining that imports require a file path. They must not silently
resolve imports against the process's current directory. A later API can
accept a virtual source path and a custom source loader for editors or
in-memory projects.

## Diagnostics and acceptance criteria

Errors should identify the original file and declaration location, with the
import chain when a dependency fails. Typical messages include:

```text
report.sagate:1: './models/users.sagate' does not export 'users'
report.sagate:2: import introduces duplicate local name 'normalize'; use 'as'
models.sagate:5: cannot export unknown name 'active_users'
report.sagate:1: cannot read './missing.sagate': file not found
<stdin>:1: imports require a file path; compile a .sagate file instead
```

Implementation should be accepted only after checking:

- Existing single-file programs retain their query names, types, and SQL.
- Importing an exported query and SQL function produces the expected SQL;
  dependency queries are absent from standalone output.
- Imported functions retain access to private helpers even when the importer
  defines helpers with the same names; direct private imports fail.
- Aliases, re-exports, overloaded functions/operators, and row-polymorphic
  functions behave as their defining bindings do.
- Prelude overrides stay within their module, including operator overrides.
- Lambda and `let` shadowing work without name capture; field labels and SQL
  placeholders survive resolution unchanged.
- Nested relative paths, imports from a different working directory, shared
  dependencies, and symbolic-link aliases obey the file-resolution rules.
- Missing files/exports, conflicting names, forward references, invalid
  private definitions, and cycles fail with source-facing diagnostics.
- Source-only APIs and stdin reject imports with actionable errors.

## Scope of the first version

Deliver named imports, aliases, explicit exports, re-exports through export
lists, private module scopes, deterministic file loading, and entry-file SQL
selection. This covers splitting tables, reusable query transformations, and
reports across files.

Defer namespace imports such as `import * as users`, wildcard exports,
default exports, packages, recursive modules, type declarations, and runtime
module values. Namespace access would need a deliberate resolution rule
alongside Sagate's existing `row.field` syntax and implicit row lambdas.
Separate compilation and persistent caching should follow a stable module
interface representation that preserves both types and the expression bodies
needed for SQL lowering.
