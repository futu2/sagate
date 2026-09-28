//! Module loading, name resolution, and linking.
//!
//! Each `.sagate` file is a module with a private top-level scope. This stage
//! loads the dependency graph behind a compilation entry file, rejects cycles,
//! enforces export visibility, rewrites free names so every module resolves
//! against its own scope plus the shared prelude, and flattens the result into
//! the `Program` shape the checker and SQL compiler already consume. Imports
//! never read the database and never run queries: only source files are read.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::ast::*;
use super::parser::{parse_module, prelude_source_name, resolved_prelude_bindings};

/// One SQL output of a compilation: the internal binding symbol together with
/// the source-facing name the result is reported under.
pub(crate) struct OutputBinding {
    pub(crate) symbol: String,
    pub(crate) name: String,
}

/// One module record retained for diagnostics: its source-facing display name
/// and, when it was reached through an import, the edge that reached it.
pub(crate) struct ChainModule {
    pub(crate) display: String,
    pub(crate) parent: Option<(usize, usize, String)>,
}

/// Where a linked symbol came from, kept for source-facing diagnostics.
pub(crate) struct Origin {
    pub(crate) module: usize,
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) name: String,
}

/// A linked compilation: the flat program the checker consumes, the entry
/// bindings whose queries become SQL outputs, and the origin metadata that
/// keeps diagnostics source-facing.
pub(crate) struct LinkedProgram {
    pub(crate) program: Program,
    pub(crate) outputs: Vec<OutputBinding>,
    pub(crate) origins: HashMap<String, Origin>,
    pub(crate) chain: Vec<ChainModule>,
}

impl LinkedProgram {
    /// Wrap a checker failure so a dependency error names its import chain,
    /// and rewrite internal symbols back to the spellings a user wrote.
    pub(crate) fn wrap_error(&self, error: TypeError) -> String {
        let mut message = error.to_string();
        if let Some(name) = error.definition() {
            if let Some(origin) = self.origins.get(name) {
                // Replace the internal symbol with the source-facing
                // "file:line:" prefix and original spelling.
                let internal = format!("definition '{name}'");
                let location = format!("{}:{}", origin.file, origin.line);
                let source_facing = format!("definition '{}' ({location})", origin.name);
                message = message.replace(&internal, &source_facing);
                let mut lines = Vec::new();
                let mut current = Some(origin.module);
                while let Some(id) = current {
                    let module = &self.chain[id];
                    match module.parent {
                        Some((parent, line, ref path)) => {
                            lines.push(format!(
                                "{}:{}: error in imported module '{path}':",
                                self.chain[parent].display, line
                            ));
                            current = Some(parent);
                        }
                        None => break,
                    }
                }
                if !lines.is_empty() {
                    lines.reverse();
                    message = format!("{}\n{message}", lines.join("\n"));
                }
            }
        }
        // Internal symbols never appear in source; any remaining occurrence
        // in a message names a written binding, so restore that spelling.
        for (symbol, origin) in &self.origins {
            if message.contains(symbol.as_str()) {
                message = message.replace(symbol.as_str(), &origin.name);
            }
        }
        message
    }
}

struct LoadedModule {
    display: String,
    parsed: ParsedModule,
    /// Import declaration index -> loaded module id.
    targets: Vec<usize>,
    parent: Option<(usize, usize, String)>,
}

struct Loader {
    modules: Vec<LoadedModule>,
    index: HashMap<PathBuf, usize>,
    /// Canonical paths of modules currently being loaded, with the path
    /// spelling used to reach them.
    stack: Vec<(PathBuf, String)>,
}

/// Load the dependency graph rooted at `entry`. Modules register in post-order,
/// so dependencies always precede their importers and a diamond dependency is
/// loaded once. Module identity is the canonical path, which deduplicates `..`
/// segments and symbolic links.
pub(crate) fn link_file(entry: &Path) -> Result<LinkedProgram, String> {
    let written = entry.display().to_string();
    let mut loader = Loader {
        modules: Vec::new(),
        index: HashMap::new(),
        stack: Vec::new(),
    };
    let entry_id = loader.load(entry, &written, None)?;
    // The entry module is the root; it has no importing parent edge.
    loader.modules[entry_id].parent = None;
    loader.link(entry_id)
}

fn read_error(error: &std::io::Error) -> String {
    if error.kind() == ErrorKind::NotFound {
        "file not found".to_owned()
    } else {
        error.to_string()
    }
}

fn cannot_read(written: &str, importer: Option<(&str, usize)>, reason: &str) -> String {
    match importer {
        Some((display, line)) => format!("{display}:{line}: cannot read '{written}': {reason}"),
        None => format!("{written}: cannot read: {reason}"),
    }
}

impl Loader {
    fn load(&mut self, path: &Path, written: &str, site: Option<usize>) -> Result<usize, String> {
        let importer = self
            .stack
            .last()
            .map(|(_, display)| (display.clone(), site.unwrap_or(0)));
        let import_site = importer
            .as_ref()
            .map(|(display, line)| (display.as_str(), *line));
        let canonical = match fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(error) => {
                return Err(cannot_read(written, import_site, &read_error(&error)));
            }
        };
        if let Some(&id) = self.index.get(&canonical) {
            return Ok(id);
        }
        if let Some(position) = self.stack.iter().position(|(known, _)| *known == canonical) {
            let mut names: Vec<String> = self.stack[position..]
                .iter()
                .map(|(_, display)| display.clone())
                .collect();
            names.push(written.to_owned());
            let prefix = match import_site {
                Some((display, line)) => format!("{display}:{line}: "),
                None => String::new(),
            };
            return Err(format!("{prefix}import cycle:\n  {}", names.join(" -> ")));
        }
        let source = match fs::read_to_string(&canonical) {
            Ok(source) => source,
            Err(error) => return Err(cannot_read(written, import_site, &read_error(&error))),
        };
        let parsed =
            parse_module(&source, written).map_err(|error| prefix_parse_error(written, error))?;

        self.stack.push((canonical.clone(), written.to_owned()));
        let mut targets = Vec::with_capacity(parsed.imports.len());
        for import in &parsed.imports {
            let base = canonical
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            let target = base.join(&import.path);
            match self.load(&target, &import.path, Some(import.line)) {
                Ok(id) => targets.push(id),
                Err(error) => {
                    self.stack.pop();
                    return Err(match &importer {
                        Some((display, line)) => format!(
                            "{display}:{line}: error in imported module '{}':\n{error}",
                            import.path
                        ),
                        None => error,
                    });
                }
            }
        }
        self.stack.pop();

        // Parent edges are derived from the import sites once every module
        // has registered, at the start of `link`.
        let id = self.modules.len();
        self.modules.push(LoadedModule {
            display: written.to_owned(),
            parsed,
            targets,
            parent: None,
        });
        self.index.insert(canonical, id);
        Ok(id)
    }

    fn link(mut self, entry: usize) -> Result<LinkedProgram, String> {
        // Fill the parent edge with the real import line and path spelling.
        let mut edges: Vec<Option<(usize, usize, String)>> = vec![None; self.modules.len()];
        {
            let modules = &self.modules;
            for (module_index, module) in modules.iter().enumerate() {
                for (import_index, import) in module.parsed.imports.iter().enumerate() {
                    let target = module.targets[import_index];
                    if edges[target].is_none() {
                        edges[target] = Some((module_index, import.line, import.path.clone()));
                    }
                }
            }
        }
        for (module, edge) in self.modules.iter_mut().zip(edges) {
            module.parent = edge;
        }

        // Validate every module's export list up front: an export must refer
        // to a local definition or an explicit import, and the same public
        // name cannot appear twice, even when nothing imports this module.
        for module_index in 0..self.modules.len() {
            if let Err(error) = self.exports_of(module_index) {
                return Err(self.with_import_chain(module_index, error));
            }
        }

        // Validate imports against export tables before rewriting, so
        // visibility failures name the written path and line.
        for module in self.modules.iter() {
            for (import_index, import) in module.parsed.imports.iter().enumerate() {
                let target_index = module.targets[import_index];
                if let Err(error) = self.exports_of(target_index) {
                    return Err(format!("{}:{}: {}", module.display, import.line, error));
                }
                let exports = self.exports_of(target_index)?;
                if !exports.contains_key(&import.source) {
                    return Err(format!(
                        "{}:{}: '{}' does not export '{}'",
                        module.display, import.line, import.path, import.source
                    ));
                }
            }
        }

        // Compiler-internal symbol for every local definition. `$` cannot be
        // written in source identifiers, so scopes can never collide.
        let mut symbols: Vec<HashMap<String, String>> = vec![HashMap::new(); self.modules.len()];
        for (module_index, module) in self.modules.iter().enumerate() {
            for parsed in &module.parsed.bindings {
                symbols[module_index].insert(
                    parsed.binding.name.clone(),
                    format!("$m{module_index}:{}", parsed.binding.name),
                );
            }
        }

        // Resolve each module independently: prelude first, then imports,
        // then locals in definition order. References follow source order, so
        // a forward reference to a later local stays an error.
        let mut flattened: Vec<Vec<Binding>> = Vec::with_capacity(self.modules.len());
        let mut outputs = Vec::new();
        let mut origins: HashMap<String, Origin> = HashMap::new();
        for (module_index, module) in self.modules.iter().enumerate() {
            // The prelude arrives as a closed layer: every binding carries an
            // internal symbol and its body already references those symbols,
            // so a module-local override never re-points another prelude
            // binding. The scope maps written spellings to the symbols visible
            // in this module; imports and local definitions shadow them below.
            let prelude = resolved_prelude_bindings();
            let prelude_scope: HashMap<String, String> = prelude
                .iter()
                .map(|binding| {
                    (
                        prelude_source_name(&binding.name).to_owned(),
                        binding.name.clone(),
                    )
                })
                .collect();
            let mut scope = prelude_scope.clone();
            let mut module_bindings: Vec<Binding> = prelude;
            let mut seen: HashSet<String> = HashSet::new();
            // Imports, checked against duplicate local names.
            for (import_index, import) in module.parsed.imports.iter().enumerate() {
                let target_index = module.targets[import_index];
                let exports = self.exports_of(target_index)?;
                let Some(symbol) = exports.get(&import.source) else {
                    return Err(format!(
                        "{}:{}: '{}' does not export '{}'",
                        module.display, import.line, import.path, import.source
                    ));
                };
                if !seen.insert(import.local.clone()) {
                    return Err(format!(
                        "{}:{}: import introduces duplicate local name '{}'; use 'as'",
                        module.display, import.line, import.local
                    ));
                }
                scope.insert(import.local.clone(), symbol.clone());
            }
            // Local bindings in definition order.
            for parsed in &module.parsed.bindings {
                if !seen.insert(parsed.binding.name.clone()) {
                    return Err(format!(
                        "{}:{}: duplicate binding '{}'",
                        module.display, parsed.line, parsed.binding.name
                    ));
                }
                scope.insert(
                    parsed.binding.name.clone(),
                    symbols[module_index][&parsed.binding.name].clone(),
                );
            }
            // Rewrite bodies with the lexical scope, then publish symbols.
            let mut resolved = Vec::with_capacity(module.parsed.bindings.len());
            for parsed in &module.parsed.bindings {
                let mut binding = parsed.binding.clone();
                // A prelude override's own body still sees the previous
                // prelude definition under its internal symbol; later
                // definitions in this module see the override.
                let saved = prelude_scope
                    .get(&binding.name)
                    .map(|previous| scope.insert(binding.name.clone(), previous.clone()));
                binding.expr = resolve_free_names(&binding.expr, &scope, &mut Vec::new()).map_err(
                    |error| {
                        let message = format!("{}:{}: {}", module.display, parsed.line, error);
                        self.with_import_chain(module_index, message)
                    },
                )?;
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
                binding.name = symbols[module_index][&binding.name].clone();
                resolved.push(binding);
            }
            module_bindings.extend(resolved);

            flattened.push(module_bindings);
        }

        // Record origins for every user binding, keyed by internal symbol.
        for (module_index, module) in self.modules.iter().enumerate() {
            for parsed in &module.parsed.bindings {
                let symbol = &symbols[module_index][&parsed.binding.name];
                origins.insert(
                    symbol.clone(),
                    Origin {
                        module: module_index,
                        file: module.display.clone(),
                        line: parsed.line,
                        name: parsed.binding.name.clone(),
                    },
                );
            }
        }

        // Entry-file queries govern SQL output: local bindings only, kept in
        // definition order under their original spellings.
        for parsed in &self.modules[entry].parsed.bindings {
            let symbol = &symbols[entry][&parsed.binding.name];
            if could_be_query(&parsed.binding.expr) {
                outputs.push(OutputBinding {
                    symbol: symbol.clone(),
                    name: parsed.binding.name.clone(),
                });
            }
        }

        // Flatten dependency modules in load order, then the entry module.
        let mut bindings = Vec::new();
        for module_bindings in &flattened {
            bindings.extend(module_bindings.iter().cloned());
        }

        let chain = self
            .modules
            .iter()
            .map(|module| ChainModule {
                display: module.display.clone(),
                parent: module.parent.clone(),
            })
            .collect();

        Ok(LinkedProgram {
            program: Program { bindings },
            outputs,
            origins,
            chain,
        })
    }

    /// Prefix a failure inside `module_index` with the import edges that
    /// reached it, so the message names the whole chain from the entry file.
    fn with_import_chain(&self, module_index: usize, message: String) -> String {
        let mut lines = Vec::new();
        let mut current = Some(module_index);
        while let Some(id) = current {
            let module = &self.modules[id];
            match module.parent {
                Some((parent, line, ref path)) => {
                    lines.push(format!(
                        "{}:{}: error in imported module '{path}':",
                        self.modules[parent].display, line
                    ));
                    current = Some(parent);
                }
                None => break,
            }
        }
        if lines.is_empty() {
            return message;
        }
        lines.reverse();
        format!("{}\n{message}", lines.join("\n"))
    }

    fn exports_of(&self, module_index: usize) -> Result<HashMap<String, String>, String> {
        let module = &self.modules[module_index];
        let display = &module.display;
        // Reject the same public name twice, including through both export
        // forms, before any symbol is produced.
        let mut names: HashMap<String, usize> = HashMap::new();
        let mut declare = |public: &str, line: usize| -> Result<(), String> {
            if let Some(previous) = names.insert(public.to_owned(), line) {
                return Err(format!("{display}:{line}: duplicate export '{public}' (first exported at line {previous})"));
            }
            Ok(())
        };
        for parsed in &module.parsed.bindings {
            if parsed.exported {
                declare(&parsed.binding.name, parsed.line)?;
            }
        }
        for export in &module.parsed.exports {
            declare(&export.public, export.line)?;
        }
        // An export must refer to a local definition or an explicit import.
        // Imports re-exported under a new name resolve to the target symbol.
        let mut local_or_imported: HashMap<String, String> = HashMap::new();
        for (import_index, import) in module.parsed.imports.iter().enumerate() {
            let target_index = module.targets[import_index];
            let target_exports = self.exports_of(target_index)?;
            if let Some(symbol) = target_exports.get(&import.source) {
                local_or_imported.insert(import.local.clone(), symbol.clone());
            }
        }
        for parsed in &module.parsed.bindings {
            local_or_imported.insert(
                parsed.binding.name.clone(),
                symbols_lookup(module_index, &parsed.binding.name),
            );
        }
        let mut exports: HashMap<String, String> = HashMap::new();
        for parsed in &module.parsed.bindings {
            if parsed.exported {
                exports.insert(
                    parsed.binding.name.clone(),
                    local_or_imported[&parsed.binding.name].clone(),
                );
            }
        }
        for export in &module.parsed.exports {
            let Some(symbol) = local_or_imported.get(&export.local) else {
                return Err(format!(
                    "{display}:{}: cannot export unknown name '{}'",
                    export.line, export.local
                ));
            };
            exports.insert(export.public.clone(), symbol.clone());
        }
        Ok(exports)
    }
}

fn symbols_lookup(module_index: usize, name: &str) -> String {
    format!("$m{module_index}:{name}")
}

/// True when a binding could compile to a standalone SQL query. The checker
/// decides which of these actually produce output.
fn could_be_query(expr: &Expr) -> bool {
    !matches!(
        expr,
        Expr::SqlTemplate(_)
            | Expr::Literal(_)
            | Expr::Lambda { .. }
            | Expr::Overloaded(_)
            | Expr::RowLiteral(_)
            | Expr::List(_)
            | Expr::Field(_)
    )
}

/// Rewrite free variable references to their compiler-internal symbols,
/// walking the expression with lexical scope information. Lambda parameters
/// and local `let` binders shadow outer names: references to them stay
/// unchanged, while every other free name must resolve against the given
/// scope. Row labels, accessed field names, and SQL template text are
/// preserved unchanged.
pub(super) fn resolve_free_names(
    expr: &Expr,
    scope: &HashMap<String, String>,
    bound: &mut Vec<String>,
) -> Result<Expr, String> {
    match expr {
        Expr::Var(name) => {
            // A bound reference (lambda parameter or `let` binder) keeps its
            // spelling; only free names are rewritten.
            if bound.iter().any(|bound_name| bound_name == name) {
                return Ok(expr.clone());
            }
            if let Some(symbol) = scope.get(name) {
                return Ok(Expr::Var(symbol.clone()));
            }
            // Unknown `__` primitives are rejected before flattening, so the
            // flat environment can never expose another module's privates.
            if name.starts_with("__") {
                return Err(format!("unknown primitive '{name}'"));
            }
            Err(format!("unknown name '{name}'"))
        }
        Expr::Lambda {
            param,
            annotation,
            body,
        } => {
            bound.push(param.clone());
            let body = resolve_free_names(body, scope, bound);
            bound.pop();
            Ok(Expr::Lambda {
                param: param.clone(),
                annotation: annotation.clone(),
                body: Box::new(body?),
            })
        }
        Expr::Let { name, value, body } => {
            let value = resolve_free_names(value, scope, bound)?;
            bound.push(name.clone());
            let body = resolve_free_names(body, scope, bound);
            bound.pop();
            Ok(Expr::Let {
                name: name.clone(),
                value: Box::new(value),
                body: Box::new(body?),
            })
        }
        Expr::Apply { function, argument } => Ok(Expr::Apply {
            function: Box::new(resolve_free_names(function, scope, bound)?),
            argument: Box::new(resolve_free_names(argument, scope, bound)?),
        }),
        Expr::Annotated { expr, ty } => Ok(Expr::Annotated {
            expr: Box::new(resolve_free_names(expr, scope, bound)?),
            ty: ty.clone(),
        }),
        Expr::Access { target, field } => Ok(Expr::Access {
            target: Box::new(resolve_free_names(target, scope, bound)?),
            field: field.clone(),
        }),
        Expr::RowLiteral(fields) => Ok(Expr::RowLiteral(
            fields
                .iter()
                .map(|(name, value)| Ok((name.clone(), resolve_free_names(value, scope, bound)?)))
                .collect::<Result<Vec<_>, String>>()?,
        )),
        Expr::List(elements) => Ok(Expr::List(
            elements
                .iter()
                .map(|element| resolve_free_names(element, scope, bound))
                .collect::<Result<Vec<_>, String>>()?,
        )),
        Expr::Overloaded(cases) => Ok(Expr::Overloaded(
            cases
                .iter()
                .map(|case| {
                    Ok(OverloadCase {
                        annotation: case.annotation.clone(),
                        expr: Box::new(resolve_free_names(&case.expr, scope, bound)?),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        )),
        // Literals, SQL templates, and field markers carry no names.
        other => Ok(other.clone()),
    }
}

fn prefix_parse_error(file: &str, error: String) -> String {
    // Module-declaration errors already carry the file and line.
    if error.starts_with(file) && error[file.len()..].starts_with(':') {
        return error;
    }
    if let Some(rest) = error.strip_prefix("line ") {
        if let Some((line, message)) = rest.split_once(": ") {
            if !line.is_empty() && line.chars().all(|character| character.is_ascii_digit()) {
                return format!("{file}:{line}: {message}");
            }
        }
    }
    format!("{file}: {error}")
}
