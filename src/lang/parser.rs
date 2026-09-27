use std::collections::HashMap;

use super::ast::*;
use super::checker::{is_scalar_primitive, substitute};

include!("parser/lexer.rs");
include!("parser/grammar.rs");
include!("parser/prelude.rs");

#[cfg(test)]
mod tests;
