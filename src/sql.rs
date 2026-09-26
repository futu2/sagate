//! SQL compilation and lowering.
mod compiler;
mod model;

pub use compiler::{compile, compile_with_dialect};
pub use model::CompiledQuery;

#[cfg(test)]
mod tests;
