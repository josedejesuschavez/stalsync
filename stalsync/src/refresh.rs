//! Comando `refresh`: re-escanea cada disco y reescribe su manifiesto para
//! que refleje el contenido ACTUAL del disco, sin copiar ni borrar nada.
//!
//! Para qué sirve:
//! - Reconstruir un manifiesto perdido o corrupto (es caché, se puede tirar).
//! - Materializar hashes/mtimes tras copias manuales (cp sin preservar mtime).
//! - Re-baselinear tras resolver conflictos a mano.
//!
//! ⚠ SEMÁNTICA (importante): refresh hace que los cambios pendientes dejen
//! de propagarse, porque cada disco se re-baselinea a su realidad actual:
//! - Un borrado pendiente se "olvida": el archivo volverá a copiarse desde
//!   los discos que aún lo tienen.
//! - Una modificación pendiente puede revertirse: si los otros discos tienen
//!   la versión vieja, la regla de mayoría la restaurará.
//!   Por eso el CLI pide confirmación antes de ejecutarlo.

use crate::Ctx;
use crate::manifest::{self, FileEntry};
use crate::scan::{self, State};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct DiskRefresh {
    pub label: String,
    /// Entradas que quedaron igual (size+mtime coincidían con el manifiesto).
    pub intactas: usize,
    /// Entradas rehasheadas o actualizadas (alta/modificación/mtime nuevo).
    pub actualizadas: usize,
    /// Entradas eliminadas del manifiesto (archivo ya no está en disco).
    pub eliminadas: usize,
}

pub fn refresh(disks: &[PathBuf]) -> io::Result<Vec<DiskRefresh>> {
    // Mismo lock que sync: refresh reescribe manifiestos y no debe
    // entrelazarse con un sync sobre los mismos discos.
    let _locks = crate::lock::acquire_all(disks)?;
    // Manifiestos primero (lectura rápida) y escaneo en paralelo, un hilo
    // por disco (mismo razonamiento que en sync: I/O real entre dispositivos).
    let mut olds = Vec::with_capacity(disks.len());
    for disk in disks {
        eprintln!("re-escaneando {}...", disk.display());
        olds.push(
            manifest::load(disk).ctx(|| format!("leyendo manifiesto de {}", disk.display()))?,
        );
    }
    let scans = scan::scan_all(disks, &olds, |_, _, _| {})
        .ctx(|| "re-escaneando discos".to_string())?;

    let mut reports = Vec::new();
    for (i, disk) in disks.iter().enumerate() {
        let old = &olds[i];
        let states = &scans[i];

        // El nuevo manifiesto ES el contenido actual del disco.
        let mut rebuilt: HashMap<PathBuf, FileEntry> = HashMap::new();
        let mut rep = DiskRefresh {
            label: disk
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| disk.display().to_string()),
            ..Default::default()
        };
        for (path, state) in states {
            let e = match state {
                State::Clean(e) | State::Changed(e) => e,
                State::Deleted => continue, // no existe: su entrada sale del manifiesto
            };
            // intacta = la entrada del manifiesto queda byte por byte igual;
            // actualizada = mtime/hash nuevos, o archivo que el manifiesto
            // desconocía (aunque el scan lo llame Clean por hash coincidente).
            if old.get(path) == Some(e) {
                rep.intactas += 1;
            } else {
                rep.actualizadas += 1;
            }
            rebuilt.insert(path.clone(), e.clone());
        }
        // Entradas que estaban en el manifiesto viejo y ya no: el archivo se
        // fue del disco. Ya no serán un "borrado pendiente": el disco queda
        // como si nunca lo hubiera tenido.
        rep.eliminadas = old.keys().filter(|k| !rebuilt.contains_key(*k)).count();
        manifest::save(disk, &rebuilt)
            .ctx(|| format!("escribiendo manifiesto de {}", disk.display()))?;
        reports.push(rep);
    }
    Ok(reports)
}

pub fn render(reports: &[DiskRefresh]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    writeln!(out, "✔ manifiestos actualizados a la realidad actual:").unwrap();
    for r in reports {
        writeln!(
            out,
            "  {:<10} {} entradas ({} intactas, {} actualizadas, {} eliminadas del manifiesto)",
            r.label,
            r.intactas + r.actualizadas,
            r.intactas,
            r.actualizadas,
            r.eliminadas
        )
        .unwrap();
    }
    out
}
