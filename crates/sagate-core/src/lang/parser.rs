use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use super::ast::*;
use super::modules::resolve_free_names;
use super::relational::ForeignId;

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
/// Foreign primitives keep their double-underscore spellings: the checker
/// and the SQL backend recognize their declarations by foreign id, and the
/// core prelude is the only place allowed to write them.
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

/// The embedded core prelude: backend declarations only. It is the one
/// source allowed to spell foreign primitives directly.
const CORE_PRELUDE_SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/core.sagate"));

/// The embedded standard library: the public language surface, written as
/// ordinary Sagate over the core declarations.
const STDLIB_PRELUDE_SOURCE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prelude.sagate"));

/// The parsed prelude layers merged into one module, built once. Every
/// module links against a clone of these bindings so parser instances never
/// share type-variable counters.
pub(super) fn prelude_module() -> &'static ParsedModule {
    static PRELUDE: OnceLock<ParsedModule> = OnceLock::new();
    PRELUDE.get_or_init(|| {
        let mut core = Parser::new(CORE_PRELUDE_SOURCE)
            .expect("core prelude lexes")
            .allowing_primitives()
            .parse_program()
            .expect("core prelude parses");
        let stdlib = Parser::new(STDLIB_PRELUDE_SOURCE)
            .expect("standard library lexes")
            .parse_program()
            .expect("standard library parses");
        core.bindings.extend(stdlib.bindings);
        core
    })
}

/// The prelude bindings as a closed layer: every binding carries its
/// internal symbol, and bodies reference those symbols. A module-level
/// override therefore hides a prelude name for later definitions in that
/// module without re-pointing any other prelude binding. Callers clone the
/// result so parser type-variable counters stay separate.
pub(super) fn resolved_prelude_bindings() -> Vec<Binding> {
    let parsed = prelude_module().clone().bindings;
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
