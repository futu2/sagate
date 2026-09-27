struct Parser {
    source: String,
    tokens: Vec<Token>,
    index: usize,
    next_type_variable: u32,
    lambda_body_depth: usize,
    allow_primitives: bool,
    /// The file this parser runs over, for module-level diagnostics.
    file_name: String,
}

impl Parser {
    fn new(source: &str) -> Result<Self, String> {
        Ok(Self {
            source: source.to_owned(),
            tokens: lex(source)?,
            index: 0,
            next_type_variable: 0,
            lambda_body_depth: 0,
            allow_primitives: false,
            file_name: "<source>".to_owned(),
        })
    }

    fn with_file_name(mut self, file_name: &str) -> Self {
        self.file_name = file_name.to_owned();
        self
    }

    fn line_at(&self, byte_offset: usize) -> usize {
        self.source[..byte_offset.min(self.source.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1
    }

    fn allowing_primitives(mut self) -> Self {
        self.allow_primitives = true;
        self
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

    fn parse_program(&mut self) -> Result<ParsedModule, String> {
        let mut module = ParsedModule::default();
        let mut seen_bindings = false;
        while !matches!(self.current().kind, TokenKind::Eof) {
            if self.eat_symbol(";") {
                continue;
            }
            // `import` and `export` are contextual keywords. They open a
            // declaration only in their full syntactic form; `export = 1`
            // remains an ordinary binding named `export`.
            if matches!(&self.current().kind, TokenKind::Ident(name) if name == "import")
                && self.import_looks_like_declaration()
            {
                if seen_bindings {
                    return self.module_error(
                        "imports must appear before the first binding or export",
                    );
                }
                self.parse_import(&mut module)?;
                continue;
            }
            if matches!(&self.current().kind, TokenKind::Ident(name) if name == "export")
                && self.export_looks_like_declaration()
            {
                seen_bindings = true;
                self.parse_export_declaration(&mut module)?;
                continue;
            }
            seen_bindings = true;
            let line = self.current_line();
            if self.eat_ident("let") {
                let binding = self.parse_binding()?;
                self.add_binding(&mut module, line, false, binding)?;
            } else {
                let binding = self.parse_definition()?;
                self.add_binding(&mut module, line, false, binding)?;
            }
        }
        Ok(module)
    }

    /// `import` opens a declaration when the next token starts an import
    /// list (`{` or a name followed by `as`), so a binding named `import`
    /// keeps parsing as a definition.
    fn import_looks_like_declaration(&self) -> bool {
        match self.tokens.get(self.index + 1).map(|token| &token.kind) {
            Some(TokenKind::Symbol(symbol)) => symbol == "{",
            Some(TokenKind::Ident(next)) if next == "_" => self.at_operator_section(self.index + 1),
            Some(TokenKind::Ident(next)) => next != ":" && next != "=" && next != "in",
            _ => false,
        }
    }

    /// `export` opens a declaration for `export { ... }` or
    /// `export name ... = ...`; anything else is a binding named `export`.
    fn export_looks_like_declaration(&self) -> bool {
        match self.tokens.get(self.index + 1).map(|token| &token.kind) {
            Some(TokenKind::Symbol(symbol)) => symbol == "{",
            Some(TokenKind::Ident(next)) if next == "_" => self.at_operator_section(self.index + 1),
            Some(TokenKind::Ident(next)) => next != ":" && next != "=" && next != "in",
            _ => false,
        }
    }

    /// True when `index` starts an operator section `_op_`.
    fn at_operator_section(&self, index: usize) -> bool {
        matches!(self.tokens.get(index).map(|token| &token.kind), Some(TokenKind::Ident(name)) if name == "_")
            && matches!(self.tokens.get(index + 1).map(|token| &token.kind), Some(TokenKind::Symbol(operator)) if is_infix_operator(operator))
            && matches!(self.tokens.get(index + 2).map(|token| &token.kind), Some(TokenKind::Ident(name)) if name == "_")
    }

    fn module_error<T>(&self, message: impl Into<String>) -> Result<T, String> {
        Err(format!("{}:{}: {}", self.file_name, self.current_line(), message.into()))
    }

    fn current_line(&self) -> usize {
        self.line_at(self.current().start)
    }

    fn parse_import(&mut self, module: &mut ParsedModule) -> Result<(), String> {
        let line = self.current_line();
        self.bump(); // `import`
        self.expect_symbol("{")?;
        let mut entries: Vec<(String, String)> = Vec::new();
        loop {
            if self.eat_ident("_") {
                // Operator section: `_+_ as add` or a bare `_+_`.
                let operator = match self.bump().kind {
                    TokenKind::Symbol(operator) if is_infix_operator(&operator) => operator,
                    _ => return self.module_error("expected an operator between '_' markers"),
                };
                if !self.eat_ident("_") {
                    return self.module_error("expected '_' after operator");
                }
                if self.eat_ident("as") {
                    let local = self.expect_ident()?;
                    entries.push((local, operator));
                } else {
                    entries.push((operator.clone(), operator));
                }
            } else {
                let name = self.expect_ident()?;
                self.reject_primitive(&name)?;
                let local = if self.eat_ident("as") {
                    let alias = self.expect_ident()?;
                    self.reject_primitive(&alias)?;
                    alias
                } else {
                    name.clone()
                };
                entries.push((local, name));
            }
            if self.eat_symbol(",") {
                // A trailing comma before `}` or `from` is allowed.
                if matches!(&self.current().kind, TokenKind::Symbol(symbol) if symbol == "}")
                    || matches!(&self.current().kind, TokenKind::Ident(name) if name == "from")
                {
                    break;
                }
                continue;
            } else {
                break;
            }
        }
        self.expect_symbol("}")?;
        if !self.eat_ident("from") {
            return self.module_error("expected 'from' after the import list");
        }
        let TokenKind::String(path) = self.bump().kind else {
            return self.module_error("import paths must be string literals");
        };
        // Report at the declaration's line; the current token may already sit
        // past the path on the following line.
        let import_error = |message: String| {
            Err(format!("{}:{}: {}", self.file_name, line, message))
        };
        if !path.starts_with("./") && !path.starts_with("../") {
            return import_error(format!(
                "import path '{path}' must start with './' or '../'"
            ));
        }
        if !path.ends_with(".sagate") {
            return import_error(format!("import path '{path}' must name a .sagate file"));
        }
        for (local, source) in entries {
            module.imports.push(ImportDecl {
                local,
                source,
                path: path.clone(),
                line,
            });
        }
        self.eat_symbol(";");
        Ok(())
    }

    fn parse_export_declaration(&mut self, module: &mut ParsedModule) -> Result<(), String> {
        let line = self.current_line();
        self.bump(); // `export`
        if self.eat_symbol("{") {
            loop {
                let (local, public) = if self.eat_ident("_") {
                    let operator = match self.bump().kind {
                        TokenKind::Symbol(operator) if is_infix_operator(&operator) => operator,
                        _ => {
                            return self
                                .module_error("expected an operator between '_' markers")
                        }
                    };
                    if !self.eat_ident("_") {
                        return self.module_error("expected '_' after operator");
                    }
                    if self.eat_ident("as") {
                        let public = self.expect_ident()?;
                        self.reject_primitive(&public)?;
                        (operator.clone(), public)
                    } else {
                        (operator.clone(), operator)
                    }
                } else {
                    let local = self.expect_ident()?;
                    self.reject_primitive(&local)?;
                    let public = if self.eat_ident("as") {
                        let public = self.expect_ident()?;
                        self.reject_primitive(&public)?;
                        public
                    } else {
                        local.clone()
                    };
                    (local, public)
                };
                module.exports.push(ExportDecl {
                    public,
                    local,
                    line,
                });
                if self.eat_symbol(",") {
                    if matches!(&self.current().kind, TokenKind::Symbol(symbol) if symbol == "}") {
                        break;
                    }
                    continue;
                }
                break;
            }
            self.expect_symbol("}")?;
            self.eat_symbol(";");
            return Ok(());
        }
        // `export name ... = ...` is a definition plus `export { name }`.
        // Rewind so the definition itself re-reads the name.
        let saved = self.index;
        let name = if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "_") {
            self.parse_operator_section()?
        } else {
            let name = self.expect_ident()?;
            self.reject_primitive(&name)?;
            name
        };
        self.index = saved;
        let binding = self.parse_definition()?;
        if binding.name != name {
            return self.module_error(format!(
                "export declaration name '{}' does not match defined name '{}'",
                name, binding.name
            ));
        }
        // Define-and-export publishes through the binding's own flag, not a
        // second export entry; otherwise the public name would register twice.
        self.add_binding(module, line, true, binding)?;
        Ok(())
    }

    fn add_binding(
        &self,
        module: &mut ParsedModule,
        line: usize,
        exported: bool,
        binding: Binding,
    ) -> Result<(), String> {
        let Some(index) = module
            .bindings
            .iter()
            .position(|existing| existing.binding.name == binding.name)
        else {
            module.bindings.push(ParsedBinding {
                line,
                exported,
                binding,
            });
            return Ok(());
        };
        let operator_overload = is_infix_operator(&binding.name);
        let existing_cases_are_annotated = match &module.bindings[index].binding.expr {
            Expr::Overloaded(cases) => cases.iter().all(|case| case.annotation.is_some()),
            _ => module.bindings[index].binding.annotation.is_some(),
        };
        if !operator_overload
            && (!existing_cases_are_annotated || binding.annotation.is_none())
        {
            return self.error(format!("duplicate binding '{}'", binding.name));
        }

        let existing = &mut module.bindings[index];
        let existing_expr = std::mem::replace(
            &mut existing.binding.expr,
            Expr::Literal(Literal::Null),
        );
        let existing_annotation = existing.binding.annotation.take();
        let mut cases = match existing_expr {
            Expr::Overloaded(cases) => cases,
            expr => vec![OverloadCase {
                annotation: existing_annotation,
                expr: Box::new(expr),
            }],
        };
        cases.push(OverloadCase {
            annotation: binding.annotation,
            expr: Box::new(binding.expr),
        });
        existing.binding.annotation = None;
        existing.binding.expr = Expr::Overloaded(cases);
        // Overloads are published as one name-level set.
        if exported {
            existing.exported = true;
        }
        Ok(())
    }

    fn parse_binding(&mut self) -> Result<Binding, String> {
        let name = self.parse_binding_name()?;
        let annotation = if self.eat_symbol(":") {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_symbol("=")?;
        let expr = desugar_implicit_lambda(self.parse_expr()?);
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
        // A bare declaration `name : type;` gives a structural backend
        // primitive its type. SQL scalar functions use `sql "..."` bodies.
        if annotation.is_some() && self.eat_symbol(";") {
            return Ok(Binding {
                name: name.clone(),
                annotation,
                expr: Expr::Var(name),
            });
        }
        self.expect_symbol("=")?;
        let expr = desugar_implicit_lambda(self.parse_expr()?);
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
        let name = self.expect_ident()?;
        self.reject_primitive(&name)?;
        Ok(name)
    }

    /// Double-underscore names name the backend primitive layer. Only the
    /// prelude may declare them; user programs reach them through the prelude
    /// wrappers.
    fn reject_primitive(&self, name: &str) -> Result<(), String> {
        if self.allow_primitives || !name.starts_with("__") {
            return Ok(());
        }
        self.error(format!("primitive '{name}' is reserved for the prelude"))
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
            "Direction" | "direction" => Ok(Type::Direction),
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
            self.expect_symbol("=")?;
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
            let value = desugar_implicit_lambda(self.parse_expr()?);
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
        self.lambda_body_depth += 1;
        let body_result = self.parse_expr();
        self.lambda_body_depth -= 1;
        let body = body_result?;
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
        let mut expression = self.parse_binary(0)?;
        while self.eat_symbol("&") {
            let right = self.parse_binary(0)?;
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

    /// Precedence climbing over the symbolic infix operators. Recursing at
    /// the same level keeps the language's right-associative reading; the
    /// levels only decide which operator binds tighter.
    fn parse_binary(&mut self, min_precedence: u8) -> Result<Expr, String> {
        let mut expression = self.parse_application()?;
        while let Some(precedence) = self.current_infix_precedence() {
            if precedence < min_precedence {
                break;
            }
            let TokenKind::Symbol(operator) = self.bump().kind else {
                unreachable!("current_infix_precedence only matches symbols");
            };
            let right = self.parse_binary(precedence)?;
            expression = Expr::Apply {
                function: Box::new(Expr::Apply {
                    function: Box::new(Expr::Var(operator)),
                    argument: Box::new(expression),
                }),
                argument: Box::new(right),
            };
        }
        Ok(expression)
    }

    fn current_infix_precedence(&self) -> Option<u8> {
        match &self.current().kind {
            TokenKind::Symbol(operator) => infix_precedence(operator),
            _ => None,
        }
    }

    fn parse_application(&mut self) -> Result<Expr, String> {
        let mut expression = self.parse_atom()?;
        loop {
            // A dot glued to the expression is field access. A spaced dot is
            // not part of the expression: it starts a new whitespace argument,
            // which is the `.field` spelling of an implicit row function.
            if matches!(&self.current().kind, TokenKind::Symbol(value) if value == ".")
                && !self.whitespace_before_current()
            {
                self.bump();
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
                    // A braced row literal argument is a row-function payload:
                    // `select {id = .id}` maps each row to its row of fields.
                    // Field markers rewrite into the payload row parameter; a
                    // literal without markers is a constant payload.
                    let literal = self.parse_braced_projection()?;
                    let (has_left, has_right) = implicit_field_markers(&literal);
                    if has_left || has_right {
                        desugar_implicit_lambda(literal)
                    } else {
                        implicit_row_lambda(literal)
                    }
                } else if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == "." || value == "(")
                {
                    // A bare field expression used as a whitespace argument
                    // is an implicit row function. This also accepts the
                    // parenthesized spelling used by query combinators.
                    desugar_implicit_lambda(self.parse_expr()?)
                } else if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == "[")
                {
                    // A bracketed list argument is a row-function payload,
                    // like a braced row literal: `order [asc .name]` maps each
                    // row to its list of tagged keys. The atom is delimited by
                    // `]`, so it does not swallow a following `&` step.
                    let list = self.parse_atom()?;
                    let (has_left, has_right) = implicit_field_markers(&list);
                    if has_left || has_right {
                        desugar_implicit_lambda(list)
                    } else {
                        implicit_row_lambda(list)
                    }
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
            TokenKind::Symbol(symbol) => matches!(symbol.as_str(), "." | "(" | "{" | "["),
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

    fn whitespace_before_current(&self) -> bool {
        if self.index == 0 {
            return false;
        }
        let previous = self.tokens[self.index - 1].start;
        self.source[previous..self.current().start]
            .chars()
            .any(char::is_whitespace)
    }

    fn parse_argument(&mut self) -> Result<Expr, String> {
        if matches!(self.current().kind, TokenKind::Symbol(ref value) if value == "{") {
            return Ok(desugar_implicit_lambda(self.parse_braced_projection()?));
        }
        Ok(desugar_implicit_lambda(self.parse_expr()?))
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        if matches!(self.current().kind, TokenKind::Ident(ref name) if name == "_")
            && matches!(self.tokens.get(self.index + 1).map(|token| &token.kind), Some(TokenKind::Symbol(operator)) if is_infix_operator(operator))
            && matches!(self.tokens.get(self.index + 2).map(|token| &token.kind), Some(TokenKind::Ident(name)) if name == "_")
        {
            return Ok(Expr::Var(self.parse_operator_section()?));
        }
        if self.eat_symbol("(") {
            let expression = self.parse_expr()?;
            self.expect_symbol(")")?;
            return Ok(desugar_implicit_lambda(expression));
        }
        if matches!(self.current().kind, TokenKind::Symbol(ref symbol) if symbol == "{") {
            return self.parse_braced_projection();
        }
        if self.eat_symbol("[") {
            let mut elements = Vec::new();
            if !self.eat_symbol("]") {
                loop {
                    elements.push(self.parse_expr()?);
                    if self.eat_symbol("]") {
                        break;
                    }
                    self.expect_symbol(",")?;
                }
            }
            return Ok(Expr::List(elements));
        }
        if self.eat_symbol(".") {
            return Ok(Expr::Field(self.expect_ident()?));
        }
        match self.bump().kind {
            TokenKind::Ident(value) if value == "true" => Ok(Expr::Literal(Literal::Bool(true))),
            TokenKind::Ident(value) if value == "false" => Ok(Expr::Literal(Literal::Bool(false))),
            TokenKind::Ident(value) if value == "null" => Ok(Expr::Literal(Literal::Null)),
            TokenKind::Ident(value) if value == "sql" => match self.bump().kind {
                TokenKind::String(template) => Ok(Expr::SqlTemplate(template)),
                _ => self.error("expected a SQL template string after 'sql'"),
            },
            // Temporal literals are syntax: `date "..."` and `timestamp "..."`
            // construct typed literals directly.
            TokenKind::Ident(value) if matches!(value.as_str(), "date" | "timestamp") => {
                match self.bump().kind {
                    TokenKind::String(text) if value == "date" => {
                        Ok(Expr::Literal(Literal::Date(text)))
                    }
                    TokenKind::String(text) => Ok(Expr::Literal(Literal::Timestamp(text))),
                    _ => self.error(format!("expected a string literal after '{value}'")),
                }
            }
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

    /// Parse a row literal `{x1 = v1, x2 = v2}`. Field values are ordinary
    /// expressions; `.field` markers inside them turn the literal into a
    /// row function through the implicit-lambda rewrite of the braced
    /// argument. This one form covers select projections, aggregate
    /// projections, and plain row values.
    fn parse_braced_projection(&mut self) -> Result<Expr, String> {
        self.expect_symbol("{")?;
        let mut fields = Vec::new();
        while !self.eat_symbol("}") {
            let name = self.expect_ident()?;
            self.expect_symbol("=")?;
            let value = self.parse_expr()?;
            fields.push((name, value));
            if !self.eat_symbol(",")
                && !matches!(self.current().kind, TokenKind::Symbol(ref s) if s == "}")
            {
                return self.error("expected ',' or '}' after row field");
            }
        }
        Ok(Expr::RowLiteral(fields))
    }

}

pub(super) fn is_infix_operator(operator: &str) -> bool {    !operator.is_empty()
        && !operator
            .chars()
            .any(|character| character.is_ascii_alphanumeric() || character == '_')
        && !matches!(
            operator,
            "|>" | "=>" | "->" | "=" | ":" | "," | ";" | "." | "(" | ")" | "{" | "}" | "["
                | "]"
        )
}

/// Binding tightness of the symbolic infix operators. The scalar arithmetic
/// and comparison operators get conventional precedence; every other
/// (user-defined) operator binds loosest.
fn infix_precedence(operator: &str) -> Option<u8> {
    match operator {
        "||" => Some(1),
        "&&" => Some(2),
        "==" | "!=" | "<" | "<=" | ">" | ">=" => Some(3),
        "+" | "-" => Some(4),
        "*" | "/" | "%" => Some(5),
        operator if operator != "&" && is_infix_operator(operator) => Some(0),
        _ => None,
    }
}

/// Wrap an expression into a single-row function. Braced row literals and
/// bracketed list arguments always denote row-function payloads.
fn implicit_row_lambda(body: Expr) -> Expr {
    Expr::Lambda {
        param: "row".to_owned(),
        annotation: Some(implicit_row_type()),
        body: Box::new(body),
    }
}

/// Elaborate the row-field shorthand into an ordinary function value.
///
/// A bare `.field` refers to the first row parameter. `that.field` refers to
/// the second row parameter, so an expression containing it gets two nested
/// lambdas. The parser keeps `Field` as a short-lived marker while parsing and
/// removes it here; the rest of the compiler only has to deal with regular
/// `Access` expressions.
fn desugar_implicit_lambda(expr: Expr) -> Expr {
    // Explicit lambdas establish their own scope. In particular, a field
    // marker in an explicit lambda body must not capture an implicit row
    // parameter introduced outside that lambda.
    if matches!(expr, Expr::Lambda { .. }) {
        return expr;
    }
    let (has_left, has_right) = implicit_field_markers(&expr);
    if !has_left && !has_right {
        return expr;
    }

    if has_right {
        let left_param = "row_left".to_owned();
        let right_param = "row_right".to_owned();
        let body = rewrite_implicit_fields(expr, &left_param, &right_param);
        Expr::Lambda {
            param: left_param,
            annotation: Some(implicit_row_type()),
            body: Box::new(Expr::Lambda {
                param: right_param,
                annotation: Some(implicit_row_type()),
                body: Box::new(body),
            }),
        }
    } else {
        let param = "row".to_owned();
        Expr::Lambda {
            param: param.clone(),
            annotation: Some(implicit_row_type()),
            body: Box::new(rewrite_implicit_fields(expr, &param, "that")),
        }
    }
}

fn implicit_row_type() -> Type {
    Type::RowType(Box::new(RowExpr::Variable(0)))
}

fn implicit_field_markers(expr: &Expr) -> (bool, bool) {
    match expr {
        Expr::Field(_) => (true, false),
        Expr::RowLiteral(fields) => fields.iter().fold((false, false), |acc, (_, value)| {
            let markers = implicit_field_markers(value);
            (acc.0 || markers.0, acc.1 || markers.1)
        }),
        Expr::List(elements) => elements.iter().fold((false, false), |acc, element| {
            let markers = implicit_field_markers(element);
            (acc.0 || markers.0, acc.1 || markers.1)
        }),
        Expr::Access { target, .. } => {
            let right = matches!(target.as_ref(), Expr::Var(name) if name == "that");
            let (left, nested_right) = implicit_field_markers(target);
            (left, right || nested_right)
        }
        Expr::Apply { function, argument } => {
            let (left_a, right_a) = implicit_field_markers(function);
            let (left_b, right_b) = implicit_field_markers(argument);
            (left_a || left_b, right_a || right_b)
        }
        Expr::Let { value, body, .. } => {
            let (left_a, right_a) = implicit_field_markers(value);
            let (left_b, right_b) = implicit_field_markers(body);
            (left_a || left_b, right_a || right_b)
        }
        Expr::Annotated { expr, .. } => implicit_field_markers(expr),
        Expr::Overloaded(cases) => cases.iter().fold((false, false), |acc, case| {
            let markers = implicit_field_markers(&case.expr);
            (acc.0 || markers.0, acc.1 || markers.1)
        }),
        // Existing compatibility nodes contain already-resolved field names,
        // so there is no marker to elaborate in them.
        _ => (false, false),
    }
}

fn rewrite_implicit_fields(expr: Expr, left_param: &str, right_param: &str) -> Expr {
    match expr {
        Expr::Field(field) => Expr::Access {
            target: Box::new(Expr::Var(left_param.to_owned())),
            field,
        },
        Expr::RowLiteral(fields) => Expr::RowLiteral(
            fields
                .into_iter()
                .map(|(name, value)| {
                    (
                        name,
                        rewrite_implicit_fields(value, left_param, right_param),
                    )
                })
                .collect(),
        ),
        Expr::List(elements) => Expr::List(
            elements
                .into_iter()
                .map(|element| rewrite_implicit_fields(element, left_param, right_param))
                .collect(),
        ),
        Expr::Access { target, field } => {
            if matches!(target.as_ref(), Expr::Var(name) if name == "that") {
                Expr::Access {
                    target: Box::new(Expr::Var(right_param.to_owned())),
                    field,
                }
            } else {
                Expr::Access {
                    target: Box::new(rewrite_implicit_fields(*target, left_param, right_param)),
                    field,
                }
            }
        }
        Expr::Apply { function, argument } => Expr::Apply {
            function: Box::new(rewrite_implicit_fields(*function, left_param, right_param)),
            argument: Box::new(rewrite_implicit_fields(*argument, left_param, right_param)),
        },
        Expr::Let { name, value, body } => Expr::Let {
            name,
            value: Box::new(rewrite_implicit_fields(*value, left_param, right_param)),
            body: Box::new(rewrite_implicit_fields(*body, left_param, right_param)),
        },
        Expr::Annotated { expr, ty } => Expr::Annotated {
            expr: Box::new(rewrite_implicit_fields(*expr, left_param, right_param)),
            ty,
        },
        Expr::Overloaded(cases) => Expr::Overloaded(
            cases
                .into_iter()
                .map(|mut case| {
                    case.expr = Box::new(rewrite_implicit_fields(
                        *case.expr,
                        left_param,
                        right_param,
                    ));
                    case
                })
                .collect(),
        ),
        other => other,
    }
}
