use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "stalsync",
    version,
    about = "Sincronización N-way de discos locales (sin maestro)",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sincroniza los discos para que converjan al mismo contenido
    Sync {
        /// Discos a sincronizar (mínimo 2)
        #[arg(required = true)]
        disks: Vec<PathBuf>,

        /// Muestra el plan sin modificar nada
        #[arg(long)]
        dry_run: bool,

        /// Detalle por archivo (log scrolleable)
        #[arg(short, long)]
        verbose: bool,

        /// Días de retención de la papelera (0 = desactivada)
        #[arg(long, default_value_t = stalsync::trash::retention_default())]
        trash_days: u64,

        /// No preservar permisos POSIX al copiar (por default se preservan)
        #[arg(long)]
        no_perms: bool,
    },

    /// Diagnóstico de desfase entre discos, solo lectura
    Status {
        /// Discos a comparar (mínimo 2)
        #[arg(required = true)]
        disks: Vec<PathBuf>,
    },

    /// Reescribe los manifiestos desde la realidad actual (olvida cambios pendientes)
    Refresh {
        /// Discos a rebaselinear (mínimo 2)
        #[arg(required = true)]
        disks: Vec<PathBuf>,

        /// No pide confirmación interactiva
        #[arg(short, long)]
        yes: bool,
    },
}

/// Confirma una operación destructiva. Con --yes o stdin no interactivo, no pregunta.
fn confirm(yes: bool, warning: &str) -> bool {
    if yes {
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
    matches!(
        line.trim().to_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    )
}

fn validate_disks(disks: &[PathBuf]) {
    if disks.len() < 2 {
        eprintln!("error: se necesitan al menos 2 discos");
        std::process::exit(2);
    }
    for d in disks {
        if !d.is_dir() {
            eprintln!("error: {} no existe o no es un directorio", d.display());
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Sync {
            disks,
            dry_run,
            verbose,
            trash_days,
            no_perms,
        } => {
            validate_disks(&disks);
            let opts = stalsync::sync::Options {
                dry_run,
                verbose,
                trash_days,
                preserve_perms: !no_perms,
            };
            if let Err(e) = stalsync::sync::run(&disks, &opts) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Refresh { disks, yes } => {
            validate_disks(&disks);
            if !confirm(
                yes,
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
        Commands::Status { disks } => {
            validate_disks(&disks);
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
    }
}
