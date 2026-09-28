mod ast;
mod checker;
mod modules;
mod parser;
mod relational;

pub(crate) use ast::{Column, Expr, Literal, Mapper, MapperAxis, Program, Row, Type};
pub(crate) use checker::{flatten_apply, substitute, type_check};
pub(crate) use modules::{link_file, LinkedProgram, OutputBinding};
pub(crate) use parser::parse;
pub(crate) use relational::{
    aggregate_row_fields, foreign_declarations, join_row, mapper_of, row_literal_fields,
    AggregateField, AggregateOp, ForeignId, ForeignOps,
};
