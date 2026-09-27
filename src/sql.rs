//! SQL compilation and lowering.
mod compiler;
mod model;

#[cfg(test)]
pub(crate) use compiler::compile;
pub(crate) use compiler::{compile_linked_with_dialect, compile_with_dialect};
pub use model::CompiledQuery;

#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod tests;
