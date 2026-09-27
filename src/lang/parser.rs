use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use super::ast::*;
use super::modules::resolve_free_names;
use super::relational::Intrinsic;

include!("parser/lexer.rs");
include!("parser/grammar.rs");
include!("parser/prelude.rs");

#[cfg(test)]
mod tests;

/// The flat-name prefix for prelude symbols. `$` cannot be written in source
/// identifiers, so prelude symbols can never collide with module or user
/// names.
const PRELUDE_SYMBOL_PREFIX: &str = "$prelude:";

/// The internal flat name of the prelude binding written as `source`.
/// Structural `__` primitives keep their spellings: the checker and the SQL
/// backend recognize them by name.
pub(super) fn prelude_symbol(source: &str) -> String {
    if source.starts_with("__") {
        source.to_owned()
    } else {
        format!("{PRELUDE_SYMBOL_PREFIX}{source}")
    }
}

/// The written prelude spelling an internal prelude symbol was built from.
pub(super) fn prelude_source_name(symbol: &str) -> &str {
    symbol.strip_prefix(PRELUDE_SYMBOL_PREFIX).unwrap_or(symbol)
}

/// The parsed prelude, built once. Every module links against a clone of
/// these bindings so parser instances never share type-variable counters.
pub(super) fn prelude_module() -> &'static ParsedModule {
    static PRELUDE: OnceLock<ParsedModule> = OnceLock::new();
    PRELUDE.get_or_init(|| {
        Parser::new(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prelude.sagate"
        )))
        .expect("prelude lexes")
        .allowing_primitives()
        .parse_program()
        .expect("prelude parses")
    })
}

/// The prelude bindings as a closed layer: structural operator spellings are
/// resolved to their `__` primitives, every binding carries its internal
/// symbol, and bodies reference those symbols. A module-level override
/// therefore hides a prelude name for later definitions in that module
/// without re-pointing any other prelude binding. Callers clone the result
/// so parser type-variable counters stay separate.
pub(super) fn resolved_prelude_bindings() -> Vec<Binding> {
    let mut parsed = prelude_module().clone().bindings;
    let definitions: HashMap<String, Expr> = parsed
        .iter()
        .map(|entry| (entry.binding.name.clone(), entry.binding.expr.clone()))
        .collect();
    for entry in &mut parsed {
        resolve_prelude_operations(&mut entry.binding.expr, &definitions)
            .expect("prelude resolves");
    }
    let scope: HashMap<String, String> = parsed
        .iter()
        .map(|entry| {
            let source = entry.binding.name.clone();
            let symbol = prelude_symbol(&source);
            (source, symbol)
        })
        .collect();
    parsed
        .into_iter()
        .map(|entry| {
            let mut binding = entry.binding;
            binding.name = scope[&binding.name].clone();
            binding.expr = resolve_free_names(&binding.expr, &scope, &mut Vec::new())
                .expect("prelude resolves");
            binding
        })
        .collect()
}

/// Parse one source file into its module shape without reading other files.
pub(super) fn parse_module(source: &str, file_name: &str) -> Result<ParsedModule, String> {
    let mut parser = Parser::new(source)?.with_file_name(file_name);
    parser.parse_program()
}
