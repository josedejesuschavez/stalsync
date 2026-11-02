//! Ejecución de la pasada de sincronización completa.
//!
//! Tres modos de salida, siguiendo la convención de rsync/rclone/cargo:
//! - TTY:      spinner de escaneo + barra de progreso redibujada in-place.
//! - No TTY:   silencioso durante la pasada (apto para logs/CI); resumen al final.
//! - verbose:  una línea por archivo (log scrolleable), independiente del TTY.

use crate::manifest::{self, FileEntry};
use crate::merge::{self, Action};
use crate::scan;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Instant;

const COPY_BUF: usize = 256 * 1024;

pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
}

/// Salida de progreso: envuelve la barra (si hay TTY) y el modo verbose.
struct Ui {
    bar: Option<ProgressBar>,
    verbose: bool,
}

impl Ui {
    fn new(verbose: bool) -> Self {
        let bar = if std::io::stderr().is_terminal() && !verbose {
            let bar = ProgressBar::new(0);
            bar.set_style(
                ProgressStyle::with_template("{spinner:.green} {msg}")
                    .unwrap()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
            );
            Some(bar)
        } else {
            None
        };
        Ui { bar, verbose }
    }

    /// Mensaje de fase (escaneo, plan) en la barra o en stderr.
    fn phase(&self, msg: impl Into<String>) {
        match &self.bar {
            Some(bar) => bar.set_message(msg.into()),
            None => {
                if self.verbose {
                    eprintln!("{}", msg.into());
                }
            }
        }
    }

    /// Detalle por archivo: línea de log solo en verbose.
    fn file(&self, msg: impl Into<String>) {
        if self.verbose {
            eprintln!("{}", msg.into());
        }
    }

    /// Prepara la barra para la fase de copia con un total conocido.
    fn start_copy(&self, total_bytes: u64) {
        if let Some(bar) = &self.bar {
            bar.set_length(total_bytes);
            bar.set_position(0);
            bar.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta}) {msg}",
                )
                .unwrap()
                .progress_chars("█▉▊▋▌▍▎▏  "),
            );
        }
    }

    fn progress(&self, bytes: u64, msg: impl Into<String>) {
        match &self.bar {
            Some(bar) => {
                bar.inc(bytes);
                bar.set_message(msg.into());
            }
            None => self.file(msg),
        }
    }

    fn finish(&self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }
}

pub fn run(disks: &[PathBuf], opts: &Options) -> io::Result<()> {
    let started = Instant::now();
    let ui = Ui::new(opts.verbose);

    // 1. Cargar manifiestos y escanear.
    let mut manifests = Vec::new();
    let mut scans = Vec::new();
    for disk in disks {
        let manifest = manifest::load(disk)?;
        ui.phase(format!("escaneando {}...", disk.display()));
        scans.push(scan::scan_disk(disk, &manifest)?);
        manifests.push(manifest);
    }

    // 2. Merge N-way.
    let result = merge::merge(disks, &scans);

    // Estadísticas del plan.
    let mut copies = 0usize;
    let mut plan_bytes = 0u64;
    let mut deletes = 0usize;
    let mut conflicts = 0usize;
    for a in &result.actions {
        match a {
            Action::Copy { to, .. } => {
                copies += to.len();
            }
            Action::Delete { .. } => deletes += 1,
            Action::RenameConflict { .. } => conflicts += 1,
        }
    }
    // Bytes a transferir: tamaño acordado × destinos (sin contar renombres,
    // que no transfieren contenido).
    for a in &result.actions {
        if let Action::Copy { path, to, .. } = a {
            if let Some(e) = result.agreed.get(path) {
                plan_bytes += e.size * to.len() as u64;
            }
        }
    }

    println!(
        "plan: {copies} copias ({}), {deletes} borrados, {conflicts} conflictos, {} rutas en estado acordado",
        fmt_bytes(plan_bytes),
        result.agreed.len()
    );

    if opts.dry_run {
        for a in &result.actions {
            println!("{a:?}");
        }
        ui.finish();
        return Ok(());
    }

    if copies > 0 {
        ui.start_copy(plan_bytes);
    }

    // 3. Ejecutar acciones.
    let mut copied_bytes = 0u64;
    let mut copied_files = 0usize;
    for a in &result.actions {
        match a {
            Action::Copy { path, from, to } => {
                let src = disks[*from].join(path);
                let targets: Vec<PathBuf> = to.iter().map(|i| disks[*i].join(path)).collect();
                let entry = result
                    .agreed
                    .get(path)
                    .expect("toda copia tiene entrada acordada")
                    .clone();
                ui.file(format!("copiando {} -> {} disco(s)", path.display(), to.len()));
                copy_one(&src, &targets, &entry, &ui)?;
                copied_bytes += entry.size * to.len() as u64;
                copied_files += to.len();
            }
            Action::Delete { path, disks: ds } => {
                for i in ds {
                    let p = disks[*i].join(path);
                    ui.file(format!("borrando {}", p.display()));
                    match fs::remove_file(&p) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            Action::RenameConflict { disk: i, from, to } => {
                let old = disks[*i].join(from);
                let new = disks[*i].join(to);
                ui.phase(format!("conflicto: {} -> {}", old.display(), new.display()));
                fs::rename(&old, &new)?;
            }
        }
    }

    // 4. Si todo salió bien, reescribir los manifiestos con el estado acordado.
    for disk in disks {
        manifest::save(disk, &result.agreed)?;
    }

    // 5. Resumen final: aquí se anuncian los conflictos, no enterrados en scroll.
    ui.finish();
    let elapsed = started.elapsed();
    println!(
        "✔ completo en {:.1}s — {copied_files} archivos copiados ({}), {deletes} borrados, {conflicts} conflictos",
        elapsed.as_secs_f64(),
        fmt_bytes(copied_bytes),
    );
    if conflicts > 0 {
        println!("⚠ versiones perdedoras guardadas como *.conflict-<disco>-<ts> (no sincronizadas):");
        for a in &result.actions {
            if let Action::RenameConflict { from, to, .. } = a {
                println!("  {} -> {}", from.display(), to.display());
            }
        }
    }
    Ok(())
}

/// Lee el origen una vez y escribe a todos los destinos en el mismo bucle.
/// Preserva mtime y hace fsync por archivo.
fn copy_one(src: &std::path::Path, targets: &[PathBuf], entry: &FileEntry, ui: &Ui) -> io::Result<()> {
    let mut input = File::open(src)?;

    let mut outputs: Vec<File> = Vec::with_capacity(targets.len());
    for t in targets {
        if let Some(parent) = t.parent() {
            fs::create_dir_all(parent)?;
        }
        outputs.push(File::create(t)?);
    }

    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for o in &mut outputs {
            o.write_all(&buf[..n])?;
        }
        ui.progress(n as u64, src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    }

    for (o, t) in outputs.into_iter().zip(targets) {
        o.sync_all()?;
        drop(o);
        // Preservar mtime acordado.
        let secs = entry.mtime_ns.div_euclid(1_000_000_000);
        let nanos = entry.mtime_ns.rem_euclid(1_000_000_000) as u32;
        filetime::set_file_mtime(t, filetime::FileTime::from_unix_time(secs, nanos))?;
        // Verificación mínima: tamaño.
        let got = fs::metadata(t)?.len();
        if got != entry.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tamaño inválido tras copiar {}: {got} != {}", t.display(), entry.size),
            ));
        }
    }
    Ok(())
}

fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
