//! Public entry points for compiling Sagate source into SQL.
//!
//! The parser, type checker, and SQL lowering modules stay private so their
//! implementation details can evolve without expanding the crate's API.

mod lang;
mod sql;

pub use sql::CompiledQuery;

/// Parse, type-check, and compile Sagate source using the ANSI SQL dialect.
pub fn compile_source(source: &str) -> Result<Vec<CompiledQuery>, String> {
    compile_source_with_dialect(source, "ansi")
}

/// Parse, type-check, and compile Sagate source for a named SQL dialect.
pub fn compile_source_with_dialect(
    source: &str,
    dialect: &str,
) -> Result<Vec<CompiledQuery>, String> {
    let program = lang::parse(source)?;
    sql::compile_with_dialect(&program, dialect)
}
