use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut repo = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: cargo agent-check [--repo <path>]\n\nValidate repository agent skills, role definitions, imports, and workflow links."
                );
                return;
            }
            "--repo" => {
                let Some(path) = args.next() else {
                    eprintln!("agent-check: --repo requires a path");
                    std::process::exit(2);
                };
                repo = Some(PathBuf::from(path));
            }
            other => {
                eprintln!("agent-check: unknown argument `{other}` (try --help)");
                std::process::exit(2);
            }
        }
    }
    let repo = repo.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|error| {
            eprintln!("agent-check: cannot read current directory: {error}");
            std::process::exit(2);
        })
    });
    let diagnostics = agent_check::validate_repo(&repo);
    if diagnostics.is_empty() {
        println!("agent-check: repository agent setup is valid");
        return;
    }
    for diagnostic in &diagnostics {
        eprintln!("{}: {}", diagnostic.path, diagnostic.message);
    }
    std::process::exit(1);
}
