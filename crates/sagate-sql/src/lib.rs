//! SQL compilation and lowering for Sagate programs.
//!
//! The relational IR, the lowering passes, and sqlglot rendering live here;
//! the language core ([`sagate-core`]) knows nothing about SQL. The compiler
//! dispatches on the foreign-operation ids declared by a program's core
//! prelude, so public names stay ordinary Sagate definitions.

mod compiler;
mod model;

#[cfg(test)]
mod compiler_tests;
#[cfg(test)]
mod module_tests;

use std::path::Path;

use sagate_core::lang::{link_file, parse, Program};

pub use model::CompiledQuery;

#[cfg(test)]
pub(crate) use compiler::compile;
pub(crate) use compiler::{compile_linked_with_dialect, compile_with_dialect};

/// Parse, type-check, and compile Sagate source using the ANSI SQL dialect.
pub fn compile_source(source: &str) -> Result<Vec<CompiledQuery>, String> {
    compile_source_with_dialect(source, "ansi")
}

/// Parse, type-check, and compile Sagate source for a named SQL dialect.
pub fn compile_source_with_dialect(
    source: &str,
    dialect: &str,
) -> Result<Vec<CompiledQuery>, String> {
    let program = parse(source)?;
    compile_with_dialect(&program, dialect)
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
    let linked = link_file(path.as_ref())?;
    compile_linked_with_dialect(&linked, dialect)
}

/// Compile an already-parsed program with the ANSI dialect. Embedders that
/// build programs with the language core directly can lower them without
/// going through the parser.
pub fn compile_program(program: &Program) -> Result<Vec<CompiledQuery>, String> {
    compile_with_dialect(program, "ansi")
}
