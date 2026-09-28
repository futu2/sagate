pub fn parse(source: &str) -> Result<Program, String> {
    let user = Parser::new(source)?.parse_program()?;
    merge_single_file(user)
}

/// Link the prelude and one anonymous user source the way single-file
/// compilation has always worked: user bindings keep their written names,
/// prelude overrides hide the original name for later definitions, and free
/// names resolve against the flat definition table.
pub(super) fn merge_single_file(user: ParsedModule) -> Result<Program, String> {
    // Source-only compilation has no file to resolve imports against, so an
    // import is rejected instead of silently reading the process's working
    // directory.
    if let Some(import) = user.imports.first() {
        return Err(format!(
            "<source>:{}: imports require a file path; compile a .sagate file instead",
            import.line
        ));
    }
    // The export list is validated the way the linker validates a file's:
    // every published name is unique and refers to a definition in this
    // source. Prelude names require a local alias before they can be
    // exported, exactly as in a module file.
    let mut exported: HashMap<String, usize> = HashMap::new();
    for parsed in &user.bindings {
        if parsed.exported {
            declare_export(&mut exported, &parsed.binding.name, parsed.line)?;
        }
    }
    let locals: HashSet<String> = user
        .bindings
        .iter()
        .map(|parsed| parsed.binding.name.clone())
        .collect();
    for export in &user.exports {
        declare_export(&mut exported, &export.public, export.line)?;
        if !locals.contains(&export.local) {
            return Err(format!(
                "<source>:{}: cannot export unknown name '{}'",
                export.line, export.local
            ));
        }
    }
    let mut bindings: Vec<Binding> = resolved_prelude_bindings();
    // Written spellings resolve to the prelude's internal symbols; user
    // bindings keep their written names and shadow the prelude below.
    let prelude_scope: HashMap<String, String> = bindings
        .iter()
        .map(|binding| {
            (
                prelude_source_name(&binding.name).to_owned(),
                binding.name.clone(),
            )
        })
        .collect();
    let mut scope = prelude_scope.clone();
    for parsed in &user.bindings {
        scope.insert(
            parsed.binding.name.clone(),
            parsed.binding.name.clone(),
        );
    }
    for parsed in user.bindings {
        let mut binding = parsed.binding;
        // A prelude override's own body still sees the previous prelude
        // definition under its internal symbol; later user definitions see
        // the override.
        let saved = prelude_scope.get(&binding.name).map(|previous| {
            scope.insert(binding.name.clone(), previous.clone())
        });
        binding.expr = resolve_free_names(&binding.expr, &scope, &mut Vec::new())
            .map_err(|error| format!("<source>:{}: {error}", parsed.line))?;
        if let Some(saved) = saved {
            match saved {
                Some(previous) => {
                    scope.insert(binding.name.clone(), previous);
                }
                None => {
                    scope.remove(&binding.name);
                }
            }
        }
        bindings.push(binding);
    }
    Ok(Program { bindings })
}

fn declare_export(
    exported: &mut HashMap<String, usize>,
    public: &str,
    line: usize,
) -> Result<(), String> {
    if let Some(previous) = exported.insert(public.to_owned(), line) {
        return Err(format!(
            "<source>:{line}: duplicate export '{public}' (first exported at line {previous})"
        ));
    }
    Ok(())
}
