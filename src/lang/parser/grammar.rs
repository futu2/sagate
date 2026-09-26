struct Parser {
    source: String,
    tokens: Vec<Token>,
    index: usize,
    next_type_variable: u32,
}

impl Parser {
    fn new(source: &str) -> Result<Self, String> {
        Ok(Self {
            source: source.to_owned(),
            tokens: lex(source)?,
            index: 0,
            next_type_variable: 0,
        })
    }

    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        self.index += 1;
        token
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, String> {
        let token = self.current();
        let line = self.source[..token.start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        Err(format!("line {line}: {}", message.into()))
    }

    fn eat_symbol(&mut self, symbol: &str) -> bool {
        if matches!(&self.current().kind, TokenKind::Symbol(value) if value == symbol) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_symbol(&mut self, symbol: &str) -> Result<(), String> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            self.error(format!("expected '{symbol}'"))
        }
    }

    fn eat_ident(&mut self, expected: &str) -> bool {
        if matches!(&self.current().kind, TokenKind::Ident(value) if value == expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.bump().kind {
            TokenKind::Ident(value) => Ok(value),
            _ => self.error("expected identifier"),
        }
    }

    fn parse_program(&mut self) -> Result<Program, String> {
        let mut tables = Vec::new();
        let mut bindings = Vec::new();
        while !matches!(self.current().kind, TokenKind::Eof) {
            if self.eat_symbol(";") {
                continue;
            }
            if self.starts_table_declaration() {
                self.eat_ident("table");
                let table = self.parse_table()?;
                if tables
                    .iter()
                    .any(|existing: &Table| existing.name == table.name)
                {
                    return self.error(format!("duplicate table '{}'", table.name));
                }
                tables.push(table);
            } else if self.eat_ident("let") {
                let binding = self.parse_binding()?;
                self.add_binding(&mut bindings, binding, false)?;
            } else if self.eat_ident("def") {
                // `def` marks a named definition as overloadable. The
                // operator section syntax remains overloadable without it.
                let binding = self.parse_binding()?;
                self.add_binding(&mut bindings, binding, true)?;
            } else if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "query") {
                return self.error(
                    "'query' declarations were removed; use an ordinary binding with a query annotation",
                );
            } else {
                let binding = self.parse_definition()?;
                self.add_binding(&mut bindings, binding, false)?;
            }
        }
        Ok(Program { tables, bindings })
    }

    fn add_binding(
        &self,
        bindings: &mut Vec<Binding>,
        binding: Binding,
        allow_named_overload: bool,
    ) -> Result<(), String> {
        let Some(index) = bindings
            .iter()
            .position(|existing| existing.name == binding.name)
        else {
            bindings.push(binding);
            return Ok(());
        };
        let operator_overload = is_infix_operator(&binding.name);
        if !operator_overload && !allow_named_overload {
            return self.error(format!("duplicate binding '{}'", binding.name));
        }

        let existing = &mut bindings[index];
        let existing_expr = std::mem::replace(&mut existing.expr, Expr::Literal(Literal::Null));
        let mut cases = match existing_expr {
            Expr::Overloaded(cases) => cases,
            expr => vec![OverloadCase {
                annotation: existing.annotation.take(),
                expr: Box::new(expr),
            }],
        };
        cases.push(OverloadCase {
            annotation: binding.annotation,
            expr: Box::new(binding.expr),
        });
        existing.annotation = None;
        existing.expr = Expr::Overloaded(cases);
        Ok(())
    }

    fn parse_table(&mut self) -> Result<Table, String> {
        let name = self.expect_ident()?;
        self.expect_symbol("{")?;
        let mut columns = Vec::new();
        while !self.eat_symbol("}") {
            let column_name = self.expect_ident()?;
            self.expect_symbol(":")?;
            let ty = self.parse_type()?;
            if columns
                .iter()
                .any(|column: &Column| column.name == column_name)
            {
                return self.error(format!(
                    "duplicate column '{column_name}' in table '{name}'"
                ));
            }
            columns.push(Column {
                name: column_name,
                ty,
            });
            if !self.eat_symbol(",")
                && !matches!(self.current().kind, TokenKind::Symbol(ref s) if s == "}")
            {
                return self.error("expected ',' or '}' after column");
            }
        }
        Ok(Table {
            name,
            row: Row::new(columns),
        })
    }

    fn parse_binding(&mut self) -> Result<Binding, String> {
        let name = self.parse_binding_name()?;
        let annotation = if self.eat_symbol(":") {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_symbol("=")?;
        let expr = self.parse_expr()?;
        self.eat_symbol(";");
        Ok(Binding {
            name,
            annotation,
            expr,
        })
    }

    fn parse_definition(&mut self) -> Result<Binding, String> {
        let name = self.parse_binding_name()?;
        let annotation = if self.eat_symbol(":") {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_symbol("=")?;
        let expr = self.parse_expr()?;
        self.eat_symbol(";");
        Ok(Binding {
            name,
            annotation,
            expr,
        })
    }

    fn parse_binding_name(&mut self) -> Result<String, String> {
        if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "_") {
            return self.parse_operator_section();
        }
        self.expect_ident()
    }

    fn parse_operator_section(&mut self) -> Result<String, String> {
        self.expect_ident()?;
        let operator = match self.bump().kind {
            TokenKind::Symbol(operator) if is_infix_operator(&operator) => operator,
            _ => return self.error("expected an operator between '_' markers"),
        };
        if !self.eat_ident("_") {
            return self.error("expected '_' after operator");
        }
        Ok(operator)
    }

    fn starts_table_declaration(&self) -> bool {
        matches!(
            (
                self.tokens.get(self.index),
                self.tokens.get(self.index + 1),
                self.tokens.get(self.index + 2)
            ),
            (
                Some(Token {
                    kind: TokenKind::Ident(name), ..
                }),
                Some(Token {
                    kind: TokenKind::Ident(_), ..
                }),
                Some(Token {
                    kind: TokenKind::Symbol(symbol), ..
                })
            ) if name == "table" && symbol == "{"
        )
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        let mut variables = HashMap::new();
        self.parse_type_with_variables(&mut variables)
    }

    fn parse_type_with_variables(
        &mut self,
        variables: &mut HashMap<String, u32>,
    ) -> Result<Type, String> {
        let left = self.parse_type_atom(variables)?;
        if self.eat_symbol("->") {
            Ok(Type::Function(
                Box::new(left),
                Box::new(self.parse_type_with_variables(variables)?),
            ))
        } else {
            Ok(left)
        }
    }

    fn parse_type_atom(&mut self, variables: &mut HashMap<String, u32>) -> Result<Type, String> {
        if self.eat_symbol("(") {
            let ty = self.parse_type_with_variables(variables)?;
            self.expect_symbol(")")?;
            return Ok(ty);
        }
        if self.eat_symbol("{") {
            let Type::Relation(row) = self.parse_row_type(variables)? else {
                unreachable!()
            };
            return Ok(Type::Record(row));
        }
        let name = self.expect_ident()?;
        match name.as_str() {
            "Int" | "int" => Ok(Type::Int),
            "String" | "string" => Ok(Type::String),
            "Bool" | "bool" => Ok(Type::Bool),
            "Float" | "float" => Ok(Type::Float),
            "Date" | "date" => Ok(Type::Date),
            "Timestamp" | "timestamp" => Ok(Type::Timestamp),
            "Any" | "any" => Ok(Type::Any),
            "Maybe" | "maybe" => Ok(Type::Maybe(Box::new(self.parse_type_atom(variables)?))),
            "List" | "list" => Ok(Type::List(Box::new(self.parse_type_atom(variables)?))),
            "Row" | "row" => Ok(type_from_bare_row_expr(
                self.parse_row_expr_atom(variables)?,
            )),
            "Table" | "table" => Ok(Type::Table),
            "KeyMapper" | "keymapper" => {
                if self.next_is_type_variable() {
                    let first = self.expect_ident()?;
                    let id = self.type_variable_id(&first, variables);
                    Ok(Type::KeyMapperWitness(Box::new(MapperType::Variable(id))))
                } else {
                    Ok(Type::KeyMapper)
                }
            }
            "ValueMapper" | "valuemapper" => {
                if self.next_is_type_variable() {
                    let first = self.expect_ident()?;
                    let id = self.type_variable_id(&first, variables);
                    Ok(Type::ValueMapperWitness(Box::new(MapperType::Variable(id))))
                } else {
                    Ok(Type::ValueMapper)
                }
            }
            "Agg" | "agg" => Ok(Type::Aggregate(Box::new(self.parse_type_atom(variables)?))),
            "Group" | "group" => Ok(Type::Group(Box::new(self.parse_type_atom(variables)?))),
            "Query" | "query" => {
                if matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == "{") {
                    self.bump();
                    self.parse_row_type(variables)
                } else if self.eat_symbol("(") {
                    let row = self.parse_row_expr(variables)?;
                    self.expect_symbol(")")?;
                    Ok(Type::RelationExpr(Box::new(row)))
                } else {
                    // `Query r` carries a named row variable. It is distinct
                    // from a concrete empty row (`query {}`), which lets a
                    // signature preserve the input schema.
                    if let TokenKind::Ident(row_name) = self.current().kind.clone() {
                        self.bump();
                        let id = self.type_variable_id(&row_name, variables);
                        Ok(Type::RelationExpr(Box::new(RowExpr::Variable(id))))
                    } else {
                        Ok(Type::Relation(Row::default()))
                    }
                }
            }
            _ if name.chars().next().is_some_and(char::is_lowercase) => {
                let id = self.type_variable_id(&name, variables);
                Ok(Type::Variable(id))
            }
            _ => self.error(format!("unknown type '{name}'")),
        }
    }

    fn parse_row_expr(&mut self, variables: &mut HashMap<String, u32>) -> Result<RowExpr, String> {
        if matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == "(" || symbol == "{")
        {
            return self.parse_row_expr_atom(variables);
        }
        let name = self.expect_ident()?;
        match name.as_str() {
            "merge" => {
                let older = self.parse_row_expr_atom(variables)?;
                let newer = self.parse_row_expr_atom(variables)?;
                Ok(RowExpr::Merge(Box::new(older), Box::new(newer)))
            }
            "mapkey" => {
                let mapper = self.parse_mapper_type(variables)?;
                let row = self.parse_row_expr_atom(variables)?;
                Ok(RowExpr::MapKey(Box::new(mapper), Box::new(row)))
            }
            "mapvalue" => {
                let mapper = self.parse_mapper_type(variables)?;
                let row = self.parse_row_expr_atom(variables)?;
                Ok(RowExpr::MapValue(Box::new(mapper), Box::new(row)))
            }
            _ => Ok(RowExpr::Variable(self.type_variable_id(&name, variables))),
        }
    }

    fn parse_row_expr_atom(
        &mut self,
        variables: &mut HashMap<String, u32>,
    ) -> Result<RowExpr, String> {
        if self.eat_symbol("(") {
            let row = self.parse_row_expr(variables)?;
            self.expect_symbol(")")?;
            Ok(row)
        } else if self.eat_symbol("{") {
            let ty = self.parse_row_type(variables)?;
            let Type::Relation(row) = ty else {
                unreachable!()
            };
            Ok(RowExpr::Concrete(row))
        } else {
            let name = self.expect_ident()?;
            Ok(RowExpr::Variable(self.type_variable_id(&name, variables)))
        }
    }

    fn parse_mapper_type(
        &mut self,
        variables: &mut HashMap<String, u32>,
    ) -> Result<MapperType, String> {
        let name = self.expect_ident()?;
        let mapper = match name.as_str() {
            "id" | "identity" => Some(Mapper::Identity),
            "snake" => Some(Mapper::Snake),
            "kebab" => Some(Mapper::Kebab),
            "camel" => Some(Mapper::Camel),
            "maybe" => Some(Mapper::Maybe),
            "list" => Some(Mapper::List),
            _ => None,
        };
        Ok(mapper.map_or_else(
            || MapperType::Variable(self.type_variable_id(&name, variables)),
            MapperType::Known,
        ))
    }

    fn parse_row_type(&mut self, variables: &mut HashMap<String, u32>) -> Result<Type, String> {
        let mut columns = Vec::new();
        while !self.eat_symbol("}") {
            let name = self.expect_ident()?;
            self.expect_symbol(":")?;
            let ty = self.parse_type_with_variables(variables)?;
            columns.push(Column { name, ty });
            if !self.eat_symbol(",")
                && !matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == "}")
            {
                return self.error("expected ',' or '}' after type field");
            }
        }
        Ok(Type::Relation(Row::new(columns)))
    }

    fn type_variable_id(&mut self, name: &str, variables: &mut HashMap<String, u32>) -> u32 {
        if let Some(id) = variables.get(name).copied() {
            return id;
        }
        let id = self.next_type_variable;
        self.next_type_variable += 1;
        variables.insert(name.to_owned(), id);
        id
    }

    fn next_is_type_variable(&self) -> bool {
        matches!(self.current().kind, TokenKind::Ident(ref name) if name.chars().next().is_some_and(char::is_lowercase))
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        if let Some(lambda) = self.try_parse_lambda()? {
            return Ok(lambda);
        }
        if self.eat_ident("let") {
            let name = self.parse_binding_name()?;
            self.expect_symbol("=")?;
            let value = self.parse_expr()?;
            if !self.eat_ident("in") {
                return self.error("expected 'in' in let expression");
            }
            let body = self.parse_expr()?;
            return Ok(Expr::Let {
                name,
                value: Box::new(value),
                body: Box::new(body),
            });
        }
        self.parse_infix()
    }

    fn try_parse_lambda(&mut self) -> Result<Option<Expr>, String> {
        let saved = self.index;
        let TokenKind::Ident(param) = self.current().kind.clone() else {
            return Ok(None);
        };
        self.bump();
        let annotation = if self.eat_symbol(":") {
            Some(self.parse_type()?)
        } else {
            None
        };
        if !self.eat_symbol("=>") {
            self.index = saved;
            return Ok(None);
        }
        let body = self.parse_expr()?;
        Ok(Some(Expr::Lambda {
            param,
            annotation,
            body: Box::new(body),
        }))
    }

    fn parse_infix(&mut self) -> Result<Expr, String> {
        let mut expression = self.parse_comparison()?;
        if self.eat_symbol(":") {
            let ty = self.parse_type()?;
            expression = Expr::Annotated {
                expr: Box::new(expression),
                ty,
            };
        }
        Ok(expression)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut expression = self.parse_infix_operand()?;
        while self.eat_symbol("&") {
            let right = self.parse_infix_operand()?;
            expression = Expr::Apply {
                function: Box::new(Expr::Apply {
                    function: Box::new(Expr::Var("&".into())),
                    argument: Box::new(expression),
                }),
                argument: Box::new(right),
            };
        }
        Ok(expression)
    }

    fn parse_infix_operand(&mut self) -> Result<Expr, String> {
        let left = self.parse_application()?;
        if matches!(&self.current().kind, TokenKind::Symbol(operator) if is_infix_operator(operator) && operator != "&")
        {
            let TokenKind::Symbol(operator) = self.bump().kind else {
                unreachable!();
            };
            let right = self.parse_comparison()?;
            Ok(Expr::Apply {
                function: Box::new(Expr::Apply {
                    function: Box::new(Expr::Var(operator)),
                    argument: Box::new(left),
                }),
                argument: Box::new(right),
            })
        } else {
            Ok(left)
        }
    }

    fn parse_application(&mut self) -> Result<Expr, String> {
        let mut expression = self.parse_atom()?;
        loop {
            if self.eat_symbol(".") {
                let field = self.expect_ident()?;
                expression = Expr::Access {
                    target: Box::new(expression),
                    field,
                };
                continue;
            }
            if self.eat_symbol("(") {
                let mut arguments = Vec::new();
                if !self.eat_symbol(")") {
                    loop {
                        arguments.push(self.parse_argument()?);
                        if self.eat_symbol(")") {
                            break;
                        }
                        self.expect_symbol(",")?;
                    }
                }
                for argument in arguments {
                    expression = Expr::Apply {
                        function: Box::new(expression),
                        argument: Box::new(argument),
                    };
                }
                continue;
            }
            if self.can_start_whitespace_argument() {
                let argument = if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == "{")
                {
                    self.parse_braced_projection()?
                } else {
                    self.parse_atom()?
                };
                expression = Expr::Apply {
                    function: Box::new(expression),
                    argument: Box::new(argument),
                };
                continue;
            }
            break;
        }
        Ok(expression)
    }

    fn can_start_whitespace_argument(&self) -> bool {
        match &self.current().kind {
            TokenKind::Ident(name) => name != "in" && !self.newline_before_current(),
            TokenKind::String(_) | TokenKind::Number(_) => true,
            TokenKind::Symbol(symbol) => matches!(symbol.as_str(), "." | "(" | "{"),
            TokenKind::Eof => false,
        }
    }

    fn newline_before_current(&self) -> bool {
        if self.index == 0 {
            return false;
        }
        let previous = self.tokens[self.index - 1].start;
        self.source[previous..self.current().start].contains('\n')
    }

    fn parse_argument(&mut self) -> Result<Expr, String> {
        if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == ".") {
            return Ok(Expr::Predicate(self.parse_predicate()?));
        }
        if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == "{") {
            return self.parse_braced_projection();
        }
        self.parse_expr()
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "_")
            && matches!(self.tokens.get(self.index + 1).map(|token| &token.kind), Some(TokenKind::Symbol(operator)) if is_infix_operator(operator))
            && matches!(self.tokens.get(self.index + 2).map(|token| &token.kind), Some(TokenKind::Ident(name)) if name == "_")
        {
            return Ok(Expr::Var(self.parse_operator_section()?));
        }
        if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "from") {
            return self.error("'from' source syntax was removed; use table \"schema\" \"name\"");
        }
        if self.eat_ident("fn") {
            let param = self.expect_ident()?;
            let annotation = if self.eat_symbol(":") {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect_symbol("=>")?;
            return Ok(Expr::Lambda {
                param,
                annotation,
                body: Box::new(self.parse_expr()?),
            });
        }
        if self.eat_symbol("(") {
            let expression = self.parse_expr()?;
            self.expect_symbol(")")?;
            return Ok(expression);
        }
        if matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == "{") {
            return self.parse_braced_projection();
        }
        if self.eat_symbol(".") {
            return Ok(Expr::Field(self.expect_ident()?));
        }
        match self.bump().kind {
            TokenKind::Ident(value) if value == "true" => Ok(Expr::Literal(Literal::Bool(true))),
            TokenKind::Ident(value) if value == "false" => Ok(Expr::Literal(Literal::Bool(false))),
            TokenKind::Ident(value) if value == "null" => Ok(Expr::Literal(Literal::Null)),
            TokenKind::Ident(value) => Ok(Expr::Var(value)),
            TokenKind::String(value) => Ok(Expr::Literal(Literal::String(value))),
            TokenKind::Number(value) if value.contains('.') => {
                Ok(Expr::Literal(Literal::Float(value)))
            }
            TokenKind::Number(value) => value
                .parse()
                .map(|value| Expr::Literal(Literal::Integer(value)))
                .map_err(|_| "invalid integer literal".to_owned()),
            _ => self.error("expected expression"),
        }
    }

    fn parse_braced_projection(&mut self) -> Result<Expr, String> {
        let aggregate = matches!(
            self.tokens.get(self.index + 3).map(|token| &token.kind),
            Some(TokenKind::Ident(_))
        );
        if aggregate {
            Ok(Expr::AggregateProjection(self.parse_aggregate_fields()?))
        } else {
            Ok(Expr::Projection(self.parse_select_fields()?))
        }
    }

    fn parse_select_fields(&mut self) -> Result<Vec<SelectField>, String> {
        self.expect_symbol("{")?;
        let mut fields = Vec::new();
        while !self.eat_symbol("}") {
            let alias = self.expect_ident()?;
            self.expect_symbol(":")?;
            let field = self.parse_field_ref()?;
            fields.push(SelectField { alias, field });
            if !self.eat_symbol(",")
                && !matches!(self.current().kind, TokenKind::Symbol(ref s) if s == "}")
            {
                return self.error("expected ',' or '}' after select field");
            }
        }
        Ok(fields)
    }

    fn parse_aggregate_fields(&mut self) -> Result<Vec<AggregateField>, String> {
        self.expect_symbol("{")?;
        let mut fields = Vec::new();
        while !self.eat_symbol("}") {
            let alias = self.expect_ident()?;
            self.expect_symbol(":")?;
            let operation = AggregateOp::Named(self.expect_ident()?);
            let field = if matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == ".")
            {
                Some(self.parse_field_ref()?)
            } else {
                None
            };
            fields.push(AggregateField {
                alias,
                operation,
                field,
            });
            if !self.eat_symbol(",")
                && !matches!(self.current().kind, TokenKind::Symbol(ref s) if s == "}")
            {
                return self.error("expected ',' or '}' after aggregate field");
            }
        }
        Ok(fields)
    }

    fn parse_field_ref(&mut self) -> Result<String, String> {
        self.expect_symbol(".")?;
        self.expect_ident()
    }

    fn parse_predicate(&mut self) -> Result<Predicate, String> {
        let field = self.parse_field_ref()?;
        let op = match self.bump().kind {
            TokenKind::Symbol(value) if is_infix_operator(&value) => CompareOp::Named(value),
            _ => return self.error("expected comparison operator"),
        };
        Ok(Predicate {
            field,
            op,
            value: self.parse_literal()?,
        })
    }

    fn parse_literal(&mut self) -> Result<Literal, String> {
        if self.eat_ident("date") {
            return match self.bump().kind {
                TokenKind::String(value) => Ok(Literal::Date(value)),
                _ => self.error("date expects a string literal"),
            };
        }
        if self.eat_ident("timestamp") {
            return match self.bump().kind {
                TokenKind::String(value) => Ok(Literal::Timestamp(value)),
                _ => self.error("timestamp expects a string literal"),
            };
        }
        match self.bump().kind {
            TokenKind::String(value) => Ok(Literal::String(value)),
            TokenKind::Number(value) if value.contains('.') => Ok(Literal::Float(value)),
            TokenKind::Number(value) => value
                .parse()
                .map(Literal::Integer)
                .map_err(|_| "invalid integer literal".to_owned()),
            TokenKind::Ident(value) if value == "true" => Ok(Literal::Bool(true)),
            TokenKind::Ident(value) if value == "false" => Ok(Literal::Bool(false)),
            TokenKind::Ident(value) if value == "null" => Ok(Literal::Null),
            _ => self.error("expected literal"),
        }
    }
}

fn is_infix_operator(operator: &str) -> bool {
    !operator.is_empty()
        && !operator
            .chars()
            .any(|character| character.is_ascii_alphanumeric() || character == '_')
        && !matches!(
            operator,
            "|>" | "=>" | "->" | "=" | ":" | "," | ";" | "." | "(" | ")" | "{" | "}"
        )
}

