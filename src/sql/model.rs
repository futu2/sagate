use std::fmt;

use crate::lang::Row;

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledQuery {
    pub name: String,
    pub sql: String,
    pub row: Row,
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
    pub(super) sql: String,
    pub(super) row: Row,
}
