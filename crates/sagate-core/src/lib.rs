//! Sagate language core.
//!
//! This crate holds everything independent of SQL policy: the parser, the
//! row-polymorphic type checker, the module loader, and the foreign-operation
//! ABI that lets backends attach lowering functions to declarations. The
//! public surface is [`lang`]; backends such as `sagate-sql` consume the
//! parsed program, the checker, and the foreign ids through it.

pub mod lang;
