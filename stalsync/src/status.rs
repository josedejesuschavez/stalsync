//! Comando `status`: diagnóstico de desfase entre discos, **solo lectura**.
//!
//! Ejecuta las mismas fases de scan + merge que un sync, pero en lugar de
//! actuar, reporta por disco: cambios locales pendientes de propagar,
//! archivos que recibirá/enviará/perderá, y conflictos. Reutiliza el merge
//! como fuente única de verdad: lo que status reporta es exactamente lo que
//! un `sync` haría.

use crate::Ctx;
use crate::manifest;
use crate::merge::{self, Action};
use crate::scan;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct DiskStatus {
    pub label: String,
    /// Altas/modificaciones locales (contra su propio manifiesto).
    pub cambios: usize,
    pub cambios_bytes: u64,
    /// Borrados locales pendientes de propagar.
    pub borrados: usize,
    /// Archivos que este disco enviará a otros.
    pub envia: usize,
    /// Archivos que este disco recibirá.
    pub recibe: usize,
    pub recibe_bytes: u64,
    /// Archivos que perderá por propagación de un borrado desde otro disco.
    pub perdera: usize,
    /// Conflictos donde su versión perdió (quedará como *.conflict-*).
    pub conflictos: usize,
}

impl DiskStatus {
    /// ¿Este disco participa en alguna acción o tiene cambios sin propagar?
    pub fn desfasado(&self) -> bool {
        self.cambios > 0
            || self.borrados > 0
            || self.recibe > 0
            || self.perdera > 0
            || self.conflictos > 0
    }
}

#[derive(Debug)]
pub struct StatusReport {
    pub disks: Vec<DiskStatus>,
    /// Rutas en el estado acordado.
    pub rutas: usize,
    /// Transferencias de archivo pendientes (un archivo × cada destino).
    pub pendientes: usize,
    /// Bytes que se escribirán en total (sumando todos los destinos).
    pub pendientes_bytes: u64,
    /// true = un sync no haría nada.
    pub convergido: bool,
}

/// Escanea y mergea sin tocar nada. Es el mismo análisis que `sync` ejecuta.
pub fn compute_status(disks: &[PathBuf]) -> io::Result<StatusReport> {
    let mut scans = Vec::new();
    for disk in disks {
        eprintln!("escaneando {}...", disk.display());
        let m = manifest::load(disk).ctx(|| format!("leyendo manifiesto de {}", disk.display()))?;
        scans.push(scan::scan_disk(disk, &m).ctx(|| format!("escaneando {}", disk.display()))?);
    }
    let result = merge::merge(disks, &scans);

    let mut dss: Vec<DiskStatus> = disks
        .iter()
        .map(|d| DiskStatus {
            label: d
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.display().to_string()),
            ..Default::default()
        })
        .collect();

    // Cambios locales por disco (medidos contra su propio manifiesto).
    for (i, s) in scans.iter().enumerate() {
        for st in s.values() {
            match st {
                scan::State::Changed(e) => {
                    dss[i].cambios += 1;
                    dss[i].cambios_bytes += e.size;
                }
                scan::State::Deleted => dss[i].borrados += 1,
                scan::State::Clean(_) => {}
            }
        }
    }

    // Lo que el merge haría, atribuido por disco.
    let mut pendientes = 0usize;
    let mut pendientes_bytes = 0u64;
    for a in &result.actions {
        match a {
            Action::Copy { path, from, to } => {
                pendientes += to.len();
                dss[*from].envia += 1;
                let size = result.agreed.get(path).map(|e| e.size).unwrap_or(0);
                pendientes_bytes += size * to.len() as u64;
                for &i in to {
                    dss[i].recibe += 1;
                    dss[i].recibe_bytes += size;
                }
            }
            Action::Delete { disks: ds, .. } => {
                pendientes += ds.len();
                for &i in ds {
                    dss[i].perdera += 1;
                }
            }
            Action::RenameConflict { disk, .. } => {
                pendientes += 1;
                dss[*disk].conflictos += 1;
            }
        }
    }

    Ok(StatusReport {
        convergido: result.actions.is_empty(),
        rutas: result.agreed.len(),
        pendientes,
        pendientes_bytes,
        disks: dss,
    })
}

/// Renderiza el reporte como texto (puro, para poder testearlo).
pub fn render(report: &StatusReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    if report.convergido {
        writeln!(
            out,
            "✔ todo sincronizado — {} discos, {} rutas gestionadas, nada pendiente",
            report.disks.len(),
            report.rutas
        )
        .unwrap();
        return out;
    }

    let n_desfasados = report.disks.iter().filter(|d| d.desfasado()).count();
    writeln!(
        out,
        "⚠ {} disco(s) desfasado(s) — {} transferencias pendientes ({}), {} rutas gestionadas",
        n_desfasados,
        report.pendientes,
        crate::sync::fmt_bytes(report.pendientes_bytes),
        report.rutas
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "{:<10} {:>18} {:>7} {:>22} {:>9} {:>10}",
        "disco", "locales (altas/bajas)", "envía", "recibe", "perderá", "conflictos"
    )
    .unwrap();
    for d in &report.disks {
        let locales = if d.cambios > 0 || d.borrados > 0 {
            format!(
                "{}/{}{}",
                d.cambios,
                d.borrados,
                if d.cambios_bytes > 0 {
                    format!(" ({})", crate::sync::fmt_bytes(d.cambios_bytes))
                } else {
                    String::new()
                }
            )
        } else {
            "0".into()
        };
        let recibe = if d.recibe > 0 {
            format!("{} ({})", d.recibe, crate::sync::fmt_bytes(d.recibe_bytes))
        } else {
            "0".into()
        };
        let marca = if d.desfasado() { "⚠" } else { "✔" };
        writeln!(
            out,
            "{:<10} {:>18} {:>7} {:>22} {:>9} {:>10}  {}",
            d.label, locales, d.envia, recibe, d.perdera, d.conflictos, marca
        )
        .unwrap();
    }
    out
}
