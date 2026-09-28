mod ast;
mod checker;
mod modules;
mod parser;
mod relational;

pub use ast::{Binding, Column, Expr, Literal, Mapper, MapperAxis, Program, Row, Type, TypeError};
pub use checker::{flatten_apply, substitute, type_check};
pub use modules::{link_file, LinkedProgram, OutputBinding};
pub use parser::parse;
pub use relational::{
    aggregate_row_fields, foreign_declarations, join_row, mapper_of, row_literal_fields,
    AggregateField, AggregateOp, Definitions, ForeignId, ForeignOps,
};
