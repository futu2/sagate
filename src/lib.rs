//! Public entry points for compiling Sagate source into SQL.
//!
//! The parser, type checker, and SQL lowering modules stay private so their
//! implementation details can evolve without expanding the crate's API.

mod lang;
mod sql;

use std::path::Path;

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

/// Parse, type-check, and compile a `.sagate` file and its relative imports
/// using the ANSI SQL dialect. Only the entry file's locally defined queries
/// become standalone SQL outputs.
pub fn compile_file(path: impl AsRef<Path>) -> Result<Vec<CompiledQuery>, String> {
    compile_file_with_dialect(path, "ansi")
}

/// Parse, type-check, and compile a `.sagate` file and its relative imports
/// for a named SQL dialect.
pub fn compile_file_with_dialect(
    path: impl AsRef<Path>,
    dialect: &str,
) -> Result<Vec<CompiledQuery>, String> {
    let linked = lang::link_file(path.as_ref())?;
    sql::compile_linked_with_dialect(&linked, dialect)
}
