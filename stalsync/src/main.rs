use std::path::PathBuf;

fn usage() {
    eprintln!("uso:");
    eprintln!("  stalsync sync <disco1> <disco2> [discoN...] [--dry-run] [-v|--verbose]");
    eprintln!("  stalsync status <disco1> <disco2> [discoN...]");
}

fn disks_from(args: &[String]) -> Vec<PathBuf> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
        std::process::exit(2);
    };

    let disks = disks_from(&args[1..]);
    if disks.len() < 2 {
        eprintln!("error: se necesitan al menos 2 discos");
        usage();
        std::process::exit(2);
    }
    for d in &disks {
        if !d.is_dir() {
            eprintln!("error: {} no existe o no es un directorio", d.display());
            std::process::exit(1);
        }
    }

    match cmd {
        "sync" => {
            let opts = stalsync::sync::Options {
                dry_run: args.iter().any(|a| a == "--dry-run"),
                verbose: args.iter().any(|a| a == "-v" || a == "--verbose"),
            };
            if let Err(e) = stalsync::sync::run(&disks, &opts) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        "status" => {
            match stalsync::status::compute_status(&disks) {
                Ok(report) => {
                    print!("{}", stalsync::status::render(&report));
                    // Exit code scriptable: 0 = convergido, 1 = desfase pendiente.
                    std::process::exit(if report.convergido { 0 } else { 1 });
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    }
}
