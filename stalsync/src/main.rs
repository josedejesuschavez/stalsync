use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("sync") || args.len() < 3 {
        eprintln!("uso: stalsync sync <disco1> <disco2> [discoN...] [--dry-run]");
        std::process::exit(2);
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let disks: Vec<PathBuf> = args[1..]
        .iter()
        .filter(|a| *a != "--dry-run")
        .map(PathBuf::from)
        .collect();

    for d in &disks {
        if !d.is_dir() {
            eprintln!("error: {} no existe o no es un directorio", d.display());
            std::process::exit(1);
        }
    }

    if let Err(e) = stalsync::sync::run(&disks, dry_run) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
