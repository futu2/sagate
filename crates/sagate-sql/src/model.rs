use std::fmt;

use sagate_core::lang::Row;
use sqlglot_rust::ast::{Cte, Statement};

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledQuery {
    pub name: String,
    pub sql: String,
    pub(crate) row: Row,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompileError {
    message: String,
}

impl CompileError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}

#[derive(Clone)]
pub(super) struct Relation {
    /// Flat `WITH` chain accumulated by pipeline steps. The statement reads
    /// the last CTE (or the base table when empty); entry attaches the list
    /// to the rendered statement. CTE bodies never carry their own `WITH` —
    /// every step lifts its input into the shared list, keeping the final
    /// SQL one flat chain for the optimizer to work with.
    pub(super) ctes: Vec<Cte>,
    pub(super) statement: Box<Statement>,
    pub(super) row: Row,
}
