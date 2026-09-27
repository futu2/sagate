use std::collections::HashMap;

use super::ast::*;
use super::relational::*;

include!("checker/validation.rs");
include!("checker/inference.rs");
include!("checker/relational.rs");
