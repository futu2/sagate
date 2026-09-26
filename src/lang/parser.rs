use std::collections::HashMap;

use super::ast::*;
use super::checker::{comparison_operator, substitute};

include!("parser/lexer.rs");
include!("parser/grammar.rs");
include!("parser/prelude.rs");

#[cfg(test)]
mod tests;
