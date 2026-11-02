//! Exclusión mutua entre procesos stalsync que operan sobre el mismo disco.
//!
//! Un lock exclusivo por disco en `<disco>/.stalsync/lock` (dentro del
//! directorio que el scan ya excluye). fs2 lo implementa con flock(2) en
//! Unix y LockFileEx en Windows: el lock lo libera el SO al morir el
//! proceso, aunque crashee a mitad de sync.
//!
//! Orden de adquisición: los discos se ordenan por ruta canónica antes de
//! bloquear. Así, dos procesos con los mismos discos en distinto orden en
//! la línea de comandos no pueden entrar en deadlock: ambos adquieren
//! siempre en el mismo orden.
//!
//! Política: fail-fast. Si otro proceso tiene el lock, se reporta el disco
//! conflictivo en vez de esperar: mejor un error claro que un sync que
//! "se queda pensando" horas porque el cron sigue corriendo.

use fs2::FileExt;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

use crate::manifest::MANIFEST_DIR;

/// Guarda los locks adquiridos. Al soltarse (fin de la función o panic),
/// los archivos se cierran y el SO libera los locks.
#[derive(Debug)]
pub struct Locks {
    // Solo existe para mantener los archivos abiertos (y con ello los
    // locks) vivos; nunca se lee de ahí el underscore.
    _held: Vec<(PathBuf, File)>,
}

/// Adquiere un lock exclusivo por disco, en orden canónico (anti-deadlock).
/// Falla con `WouldBlock` y mensaje claro si algún disco ya está en uso.
pub fn acquire_all(disks: &[PathBuf]) -> io::Result<Locks> {
    let mut ordered: Vec<&PathBuf> = disks.iter().collect();
    ordered.sort_by_key(|d| d.canonicalize().unwrap_or_else(|_| (*d).clone()));

    let mut held = Vec::with_capacity(ordered.len());
    for disk in ordered {
        let dir = disk.join(MANIFEST_DIR);
        fs::create_dir_all(&dir)?;
        let file = File::create(dir.join("lock"))?;
        if file.try_lock_exclusive().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "otro proceso stalsync está usando {} \
                     (ciérralo o espera a que termine)",
                    disk.display()
                ),
            ));
        }
        held.push((disk.clone(), file));
    }
    Ok(Locks { _held: held })
}
