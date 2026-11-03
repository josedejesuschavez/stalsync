//! Ejecución de la pasada de sincronización completa.
//!
//! Tres modos de salida, siguiendo la convención de rsync/rclone/cargo:
//! - TTY:      spinner de escaneo + barra de progreso redibujada in-place.
//! - No TTY:   silencioso durante la pasada (apto para logs/CI); resumen al final.
//! - verbose:  una línea por archivo (log scrolleable), independiente del TTY.

use crate::Ctx;
use crate::manifest::{self, FileEntry};
use crate::merge::{self, Action};
use crate::scan;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

const COPY_BUF: usize = 256 * 1024;

pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
    /// Días de retención de la papelera de borrados. 0 = unlink directo.
    pub trash_days: u64,
    /// Preservar permisos POSIX al copiar y propagar chmod. Default: true.
    /// En Windows es un no-op (no hay modes POSIX).
    pub preserve_perms: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            dry_run: false,
            verbose: false,
            trash_days: crate::trash::retention_default(),
            preserve_perms: true,
        }
    }
}

/// Estado del spinner de escaneo: totales por disco y el último punto
/// redibujado (para el throttle). Vive detrás de un Mutex porque el
/// escaneo de cada disco corre en su propio hilo.
struct ScanState {
    per_disk: Vec<(usize, u64)>,
    /// Último (archivos, bytes) con el que se redibujó el spinner.
    drawn: (usize, u64),
}

/// Salida de progreso: spinner de escaneo + una barra por disco destino
/// (MultiProgress), o log por línea en verbose. Sin TTY: silencioso.
struct Ui {
    multi: Option<MultiProgress>,
    scan: Option<ProgressBar>,
    /// Totales de escaneo por disco; los hilos de escaneo lo actualizan.
    scan_state: Mutex<ScanState>,
    /// Barras por índice de disco; solo los discos destino de copias tienen barra.
    disk_bars: Vec<Option<ProgressBar>>,
    verbose: bool,
}

impl Ui {
    fn new(n_disks: usize, verbose: bool) -> Self {
        if std::io::stderr().is_terminal() && !verbose {
            let multi = MultiProgress::new();
            let scan = multi.add(ProgressBar::new_spinner());
            scan.set_style(
                ProgressStyle::with_template("{spinner:.green} {msg}")
                    .unwrap()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
            );
            Ui {
                multi: Some(multi),
                scan: Some(scan),
                scan_state: Mutex::new(ScanState {
                    per_disk: vec![(0, 0); n_disks],
                    drawn: (0, 0),
                }),
                disk_bars: vec![None; n_disks],
                verbose,
            }
        } else {
            Ui {
                multi: None,
                scan: None,
                scan_state: Mutex::new(ScanState {
                    per_disk: vec![(0, 0); n_disks],
                    drawn: (0, 0),
                }),
                disk_bars: Vec::new(),
                verbose,
            }
        }
    }

    /// Plantilla con la etiqueta del disco HORNEADA y de ancho fijo: todo lo
    /// que precede a la barra es constante, así la barra nunca se desplaza.
    /// El nombre del archivo (variable) va al final en {wide_msg}, que indicatif
    /// trunca si no cabe en la terminal.
    fn bar_style(label: &str) -> ProgressStyle {
        // Sanitizar llaves (romperían el template) y fijar ancho con padding.
        // El color se hornea como ANSI literal: indicatif lo respeta, y Rust no
        // permite estilos dentro de especificadores de formato.
        let safe: String = label
            .chars()
            .map(|c| if c == '{' || c == '}' { '_' } else { c })
            .collect();
        let labeled = format!("\x1b[36;1m{:>8}\x1b[0m ", safe);
        let template = format!(
            "{{spinner:.green}} {}[{{bar:30.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}, ETA {{eta}}) {{wide_msg:.magenta}}",
            labeled
        );
        ProgressStyle::with_template(&template)
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  ")
    }

    /// Mensaje de fase (escaneo, plan): va al spinner.
    fn phase(&self, msg: impl Into<String>) {
        match &self.scan {
            Some(bar) => bar.set_message(msg.into()),
            None => {
                if self.verbose {
                    eprintln!("{}", msg.into());
                }
            }
        }
    }

    /// Guarda los totales del disco `i` y redibuja el spinner con el
    /// agregado de todos los discos. Se invoca desde el hilo de escaneo
    /// de cada disco. Redibuja cada 32 archivos O cada 512 MiB acumulados
    /// (lo que primero ocurra): redibujar por archivo cuesta más que el
    /// stat en algunos TTY, y con pocos archivos enormes el conteo de
    /// archivos solo nunca dispararía la actualización.
    fn scan_progress(&self, i: usize, n: usize, bytes: u64) {
        const CADA_BYTES: u64 = 512 * 1024 * 1024;
        let mut st = self.scan_state.lock().unwrap();
        st.per_disk[i] = (n, bytes);
        let files: usize = st.per_disk.iter().map(|t| t.0).sum();
        let done: u64 = st.per_disk.iter().map(|t| t.1).sum();
        if files - st.drawn.0 < 32 && done.saturating_sub(st.drawn.1) < CADA_BYTES {
            return;
        }
        st.drawn = (files, done);
        let n_disks = st.per_disk.len();
        drop(st);
        if let Some(bar) = &self.scan {
            bar.set_message(format!(
                "escaneando {n_disks} discos en paralelo... ({files} archivos, {} escaneados)",
                fmt_bytes(done)
            ));
        }
    }

    /// Detalle por archivo: línea de log solo en verbose.
    fn file(&self, msg: impl Into<String>) {
        if self.verbose {
            eprintln!("{}", msg.into());
        }
    }

    /// Crea una barra por disco destino, con su total de bytes a escribir.
    fn start_copy(&mut self, disks: &[PathBuf], totals: &[u64]) {
        if let Some(multi) = &self.multi {
            self.scan = None; // el spinner de escaneo termina aquí
            for (i, total) in totals.iter().enumerate() {
                if *total == 0 {
                    continue;
                }
                let label = disks[i]
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("disco{i}"));
                let bar = multi.add(ProgressBar::new(*total));
                bar.set_style(Self::bar_style(&label));
                self.disk_bars[i] = Some(bar);
            }
        }
    }

    /// Avanza las barras de los discos destino de esta copia. El archivo
    /// actual solo se escribe en el {wide_msg} final (truncable), nunca antes
    /// de la barra: la geometría de la barra es invariante.
    fn progress(&self, targets: &[usize], bytes: u64, current: &str) {
        for &i in targets {
            if let Some(Some(bar)) = self.disk_bars.get(i) {
                bar.inc(bytes);
                bar.set_message(current.to_string());
            }
        }
    }

    fn finish(&self) {
        if let Some(multi) = &self.multi {
            for bar in self.disk_bars.iter().flatten() {
                bar.finish_and_clear();
            }
            multi.clear().ok();
        }
    }
}

pub fn run(disks: &[PathBuf], opts: &Options) -> io::Result<()> {
    let started = Instant::now();
    // Exclusión entre procesos: dos syncs simultáneos sobre los mismos
    // discos intercalarían copias/borrados y manifiestos. El guard se
    // mantiene vivo hasta el final de la función.
    let _locks = crate::lock::acquire_all(disks)?;
    let mut ui = Ui::new(disks.len(), opts.verbose);

    // Retención de papeleras: antes de nada, purgar entradas viejas.
    // En dry-run no se toca nada (ni siquiera la papelera).
    let mut purged = 0usize;
    if !opts.dry_run {
        for disk in disks {
            purged += crate::trash::purge(disk, opts.trash_days)
                .ctx(|| format!("purgando papelera de {}", disk.display()))?;
        }
    }

    // 1. Cargar manifiestos y escanear. Los manifiestos son una lectura
    // rápida (un archivo por disco) y van secuenciales; el escaneo, que es
    // lo costoso, corre en paralelo con un hilo por disco: son dispositivos
    // físicos distintos y el I/O concurrente entre ellos es real, así el
    // tiempo total pasa de la suma de discos al disco más lento.
    let mut manifests = Vec::with_capacity(disks.len());
    for disk in disks {
        manifests.push(
            manifest::load(disk).ctx(|| format!("leyendo manifiesto de {}", disk.display()))?,
        );
    }
    ui.phase(format!("escaneando {} discos en paralelo...", disks.len()));
    let scans = scan::scan_all(disks, &manifests, |i, n, bytes| {
        ui.scan_progress(i, n, bytes);
    })
    .ctx(|| "escaneando discos".to_string())?;

    // 2. Merge N-way.
    let result = merge::merge(disks, &scans);

    // Estadísticas del plan.
    let mut copies = 0usize;
    let mut plan_bytes = 0u64;
    let mut deletes = 0usize;
    let mut conflicts = 0usize;
    let mut chmods = 0usize;
    for a in &result.actions {
        match a {
            Action::Copy { to, .. } => {
                copies += to.len();
            }
            Action::Delete { .. } => deletes += 1,
            Action::RenameConflict { .. } => conflicts += 1,
            Action::Chmod { disks, .. } => chmods += disks.len(),
        }
    }
    // Bytes a transferir por disco destino: tamaño acordado sumado a cada `to`
    // (sin contar renombres, que no transfieren contenido).
    let mut disk_totals = vec![0u64; disks.len()];
    for a in &result.actions {
        if let Action::Copy { path, to, .. } = a {
            if let Some(e) = result.agreed.get(path) {
                plan_bytes += e.size * to.len() as u64;
                for &i in to {
                    disk_totals[i] += e.size;
                }
            }
        }
    }

    println!(
        "plan: {copies} copias ({}), {deletes} borrados, {conflicts} conflictos, {chmods} chmods, {} rutas en estado acordado",
        fmt_bytes(plan_bytes),
        result.agreed.len()
    );
    let desglose: Vec<String> = disks
        .iter()
        .zip(&disk_totals)
        .filter(|(_, b)| **b > 0)
        .map(|(d, b)| format!("{}: {}", d.display(), fmt_bytes(*b)))
        .collect();
    if !desglose.is_empty() {
        println!("  por disco: {}", desglose.join(" · "));
    }

    if opts.dry_run {
        for a in &result.actions {
            println!("{a:?}");
        }
        ui.finish();
        return Ok(());
    }

    if copies > 0 {
        ui.start_copy(disks, &disk_totals);
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
                ui.file(format!(
                    "copiando {} -> {} disco(s)",
                    path.display(),
                    to.len()
                ));
                copy_one(&src, to, &targets, &entry, opts.preserve_perms, &ui).ctx(|| {
                    format!(
                        "copiando {} desde {}",
                        path.display(),
                        disks[*from].display()
                    )
                })?;
                copied_bytes += entry.size * to.len() as u64;
                copied_files += to.len();
            }
            Action::Delete { path, disks: ds } => {
                for i in ds {
                    let p = disks[*i].join(path);
                    ui.file(format!("a papelera: {}", p.display()));
                    crate::trash::remove_or_trash(&disks[*i], &p, opts.trash_days)
                        .ctx(|| format!("borrando {} en {}", p.display(), disks[*i].display()))?;
                }
            }
            Action::RenameConflict { disk: i, from, to } => {
                let old = disks[*i].join(from);
                let new = disks[*i].join(to);
                ui.phase(format!("conflicto: {} -> {}", old.display(), new.display()));
                fs::rename(&old, &new).ctx(|| {
                    format!(
                        "guardando versión perdedora {} en {}",
                        old.display(),
                        disks[*i].display()
                    )
                })?;
            }
            Action::Chmod {
                path,
                mode,
                disks: ds,
            } => {
                for i in ds {
                    let p = disks[*i].join(path);
                    ui.file(format!("chmod {:o}: {}", mode, p.display()));
                    set_mode(&p, *mode).ctx(|| {
                        format!(
                            "aplicando permisos {:o} a {} en {}",
                            mode,
                            p.display(),
                            disks[*i].display()
                        )
                    })?;
                }
            }
        }
    }

    // 4. Si todo salió bien, reescribir los manifiestos con el estado acordado.
    for disk in disks {
        manifest::save(disk, &result.agreed)
            .ctx(|| format!("escribiendo manifiesto de {}", disk.display()))?;
    }

    // 5. Resumen final: aquí se anuncian los conflictos, no enterrados en scroll.
    ui.finish();
    let elapsed = started.elapsed();
    println!(
        "✔ completo en {:.1}s — {copied_files} archivos copiados ({}), {deletes} borrados, {conflicts} conflictos, {chmods} chmods",
        elapsed.as_secs_f64(),
        fmt_bytes(copied_bytes),
    );
    if purged > 0 {
        println!(
            "🗑 {purged} entrada(s) de papelera purgada(s) (retención {} días)",
            opts.trash_days
        );
    }
    if conflicts > 0 {
        println!(
            "⚠ versiones perdedoras guardadas como *.conflict-<disco>-<ts> (no sincronizadas):"
        );
        for a in &result.actions {
            if let Action::RenameConflict { from, to, .. } = a {
                println!("  {} -> {}", from.display(), to.display());
            }
        }
    }
    Ok(())
}

/// Aplica permisos POSIX a un archivo existente. No-op en plataformas sin
/// modes POSIX (el merge nunca genera Chmod ahí, y entry.mode es None).
#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Lee el origen una vez y escribe a todos los destinos en el mismo bucle.
/// Preserva mtime y hace fsync por archivo.
fn copy_one(
    src: &std::path::Path,
    targets_idx: &[usize],
    targets: &[PathBuf],
    entry: &FileEntry,
    preserve_perms: bool,
    ui: &Ui,
) -> io::Result<()> {
    let mut input = File::open(src).ctx(|| format!("abriendo origen {}", src.display()))?;

    let mut outputs: Vec<File> = Vec::with_capacity(targets.len());
    for t in targets {
        if let Some(parent) = t.parent() {
            fs::create_dir_all(parent).ctx(|| format!("creando directorio de {}", t.display()))?;
        }
        outputs.push(File::create(t).ctx(|| format!("creando destino {}", t.display()))?);
    }

    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for (o, t) in outputs.iter_mut().zip(targets) {
            o.write_all(&buf[..n])
                .ctx(|| format!("escribiendo {}", t.display()))?;
        }
        let current = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.progress(targets_idx, n as u64, &current);
    }

    // Verificación BLAKE3 post-copia: "el hash es la verdad". Si el hash
    // acordado está vacío (manifiesto viejo), se toma el del origen leído.
    let reference = if entry.hash.is_empty() {
        crate::scan::hash_file(src)?
    } else {
        entry.hash.clone()
    };

    for (o, t) in outputs.into_iter().zip(targets) {
        o.sync_all().ctx(|| format!("fsync de {}", t.display()))?;
        drop(o);
        // Preservar mtime acordado.
        let secs = entry.mtime_ns.div_euclid(1_000_000_000);
        let nanos = entry.mtime_ns.rem_euclid(1_000_000_000) as u32;
        filetime::set_file_mtime(t, filetime::FileTime::from_unix_time(secs, nanos))
            .ctx(|| format!("fijando mtime de {}", t.display()))?;
        // Verificación de integridad: tamaño + hash BLAKE3 contra el acordado.
        // Un destino corrupto se detecta AHORA (y el sync aborta antes de
        // reescribir manifiestos: la próxima pasada lo reintenta entero).
        let got = fs::metadata(t)?.len();
        if got != entry.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "tamaño inválido tras copiar {}: {got} != {}",
                    t.display(),
                    entry.size
                ),
            ));
        }
        let got_hash =
            crate::scan::hash_file(t).ctx(|| format!("verificando hash de {}", t.display()))?;
        if got_hash != reference {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "hash BLAKE3 inválido tras copiar {}: {got_hash} != {reference} \
                     (destino corrupto o origen modificado durante la copia)",
                    t.display()
                ),
            ));
        }
        // Preservar permisos acordados (no-op si no aplica o si no hay mode).
        if preserve_perms {
            if let Some(mode) = entry.mode {
                set_mode(t, mode)
                    .ctx(|| format!("aplicando permisos {:o} a {}", mode, t.display()))?;
            }
        }
    }
    Ok(())
}

pub fn fmt_bytes(n: u64) -> String {
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
