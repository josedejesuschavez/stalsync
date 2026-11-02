use std::io::IsTerminal;
use std::path::PathBuf;

fn usage() {
    eprintln!("uso:");
    eprintln!("  stalsync sync <disco1> <disco2> [discoN...] [--dry-run] [-v|--verbose]");
    eprintln!("  stalsync status <disco1> <disco2> [discoN...]");
    eprintln!("  stalsync refresh <disco1> <disco2> [discoN...] [--yes]");
}

/// Confirma una operación destructiva. Con --yes o stdin no interactivo, no pregunta.
fn confirm(args: &[String], warning: &str) -> bool {
    if args.iter().any(|a| a == "--yes" || a == "-y") {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("{warning}");
        eprintln!("error: stdin no interactivo; confirma con --yes");
        return false;
    }
    eprintln!("{warning}");
    eprint!("¿continuar? [s/N]: ");
    use std::io::Write as _;
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes")
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
        "refresh" => {
            if !confirm(
                &args,
                "⚠ refresh re-baselinea cada disco a su contenido actual: \
                 los cambios pendientes de propagar se olvidan \
                 (un borrado pendiente hará que el archivo vuelva; \
                 una modificación pendiente puede revertirse por mayoría).",
            ) {
                std::process::exit(1);
            }
            match stalsync::refresh::refresh(&disks) {
                Ok(reports) => print!("{}", stalsync::refresh::render(&reports)),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
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
