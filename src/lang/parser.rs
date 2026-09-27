use std::collections::HashMap;

use super::ast::*;
use super::checker::substitute;
use super::relational::Intrinsic;

include!("parser/lexer.rs");
include!("parser/grammar.rs");
include!("parser/prelude.rs");

#[cfg(test)]
mod tests;
