mod ast;
mod checker;
mod parser;

pub use ast::{
    AggregateField, AggregateOp, Binding, Column, Expr, Extent, Kind, Literal, Mapper, MapperType,
    OverloadCase, Program, Row, RowExpr, Type, TypeError,
};
pub use checker::{flatten_apply, substitute, type_check};
pub use parser::parse;
