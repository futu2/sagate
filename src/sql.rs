//! SQL compilation and lowering.
mod compiler;
mod model;

#[cfg(test)]
pub(crate) use compiler::compile;
pub(crate) use compiler::compile_with_dialect;
pub use model::CompiledQuery;

#[cfg(test)]
mod tests;
