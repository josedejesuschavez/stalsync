use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("sync") || args.len() < 3 {
        eprintln!("uso: stalsync sync <disco1> <disco2> [discoN...] [--dry-run] [-v|--verbose]");
        std::process::exit(2);
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let disks: Vec<PathBuf> = args[1..]
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect();

    for d in &disks {
        if !d.is_dir() {
            eprintln!("error: {} no existe o no es un directorio", d.display());
            std::process::exit(1);
        }
    }

    let opts = stalsync::sync::Options { dry_run, verbose };
    if let Err(e) = stalsync::sync::run(&disks, &opts) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
