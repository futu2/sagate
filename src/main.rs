use std::{
    env, fs,
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
    let source = match input {
        None | Some("-") => {
            let mut input = String::new();
            if let Err(error) = io::stdin().read_to_string(&mut input) {
                eprintln!("sagate: cannot read stdin: {error}");
                return ExitCode::from(1);
            }
            input
        }
        Some(path) => match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("sagate: cannot read {path}: {error}");
                return ExitCode::from(1);
            }
        },
    };

    match sagate::parse(&source).and_then(|program| sagate::compile_with_dialect(&program, dialect))
    {
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
