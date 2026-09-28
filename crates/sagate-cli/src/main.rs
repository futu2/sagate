use std::{
    env,
    io::{self, Read},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let (dialect, input) = match args.as_slice() {
        [_, flag, dialect, path] if flag == "--dialect" => (dialect.as_str(), Some(path.as_str())),
        [_, flag, dialect] if flag == "--dialect" => (dialect.as_str(), None),
        [_, path] => ("ansi", Some(path.as_str())),
        [_] => ("ansi", None),
        _ => {
            eprintln!("usage: sagate [--dialect DIALECT] [PATH|-]");
            return ExitCode::from(2);
        }
    };
    let compile_result = match input {
        None | Some("-") => {
            let mut input = String::new();
            if let Err(error) = io::stdin().read_to_string(&mut input) {
                eprintln!("sagate: cannot read stdin: {error}");
                return ExitCode::from(1);
            }
            sagate_sql::compile_source_with_dialect(&input, dialect)
        }
        Some(path) => {
            // A file path compiles through the module loader, which follows
            // relative imports from the file's directory.
            sagate_sql::compile_file_with_dialect(path, dialect)
        }
    };

    match compile_result {
        Ok(queries) => {
            for query in queries {
                println!("-- query {}\n{};\n", query.name, query.sql);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sagate: {error}");
            ExitCode::from(1)
        }
    }
}
