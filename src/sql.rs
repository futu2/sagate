//! SQL compilation and lowering.
mod compiler;
mod model;

pub use compiler::compile;
pub use model::CompiledQuery;

#[cfg(test)]
mod tests;
