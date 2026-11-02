//! Ejecución de la pasada de sincronización completa.

use crate::manifest::{self, FileEntry};
use crate::merge::{self, Action};
use crate::scan;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const COPY_BUF: usize = 256 * 1024;

pub fn run(disks: &[PathBuf], dry_run: bool) -> io::Result<()> {
    // 1. Cargar manifiestos.
    let mut manifests = Vec::new();
    for disk in disks {
        manifests.push(manifest::load(disk)?);
    }

    // 2. Escanear cada disco (HASH_THEN_MTIME contra su propio manifiesto).
    let mut scans = Vec::new();
    for (disk, manifest) in disks.iter().zip(&manifests) {
        eprintln!("escaneando {}...", disk.display());
        scans.push(scan::scan_disk(disk, manifest)?);
    }

    // 3. Merge N-way.
    let result = merge::merge(disks, &scans);

    // Estadísticas simples.
    let mut copies = 0usize;
    let mut deletes = 0usize;
    let mut conflicts = 0usize;
    for a in &result.actions {
        match a {
            Action::Copy { to, .. } => copies += to.len(),
            Action::Delete { .. } => deletes += 1,
            Action::RenameConflict { .. } => conflicts += 1,
        }
    }
    println!(
        "plan: {} copias, {} borrados, {} conflictos, {} rutas en estado acordado",
        copies,
        deletes,
        conflicts,
        result.agreed.len()
    );

    if dry_run {
        for a in &result.actions {
            println!("{a:?}");
        }
        return Ok(());
    }

    // 4. Ejecutar acciones.
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
                eprintln!("copiando {} -> {} disco(s)", path.display(), to.len());
                copy_one(&src, &targets, &entry)?;
            }
            Action::Delete { path, disks: ds } => {
                for i in ds {
                    let p = disks[*i].join(path);
                    eprintln!("borrando {}", p.display());
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
                eprintln!("conflicto: {} -> {}", old.display(), new.display());
                fs::rename(&old, &new)?;
            }
        }
    }

    // 5. Si todo salió bien, reescribir los manifiestos con el estado acordado.
    for disk in disks {
        manifest::save(disk, &result.agreed)?;
    }
    println!("sincronización completa");
    Ok(())
}

/// Lee el origen una vez y escribe a todos los destinos en el mismo bucle.
/// Preserva mtime y hace fsync por archivo.
fn copy_one(src: &Path, targets: &[PathBuf], entry: &FileEntry) -> io::Result<()> {
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
