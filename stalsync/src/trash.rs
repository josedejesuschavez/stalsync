//! Papelera de borrados (versioning ligero).
//!
//! La política sigue siendo "propagar borrados" (los discos convergen), pero
//! la ejecución no hace unlink() directo: mueve el archivo a
//! `<disco>/.stalsync/trash/<timestamp>/<ruta-original>`. Como es el mismo
//! filesystem, es un rename instantáneo (cero bytes copiados).
//!
//! Ventajas:
//! - Un borrado accidental es reversible: el archivo sobrevive en la
//!   papelera del disco que lo perdió, con su ruta original.
//! - Restaurar = mover el archivo de vuelta; el siguiente sync lo repropaga.
//! - La papelera vive dentro de `.stalsync/`, que el escaneo ya excluye:
//!   los archivos en papelera no resucitan ni interfieren con el merge.
//!
//! Retención: cada sync purga las entradas con más de `days` días.
//! `days = 0` desactiva la papelera (unlink directo, comportamiento viejo).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::manifest::MANIFEST_DIR;
use crate::Ctx;

pub const TRASH_DIR: &str = "trash";
const RETENTION_DEFAULT_DAYS: u64 = 30;

pub fn retention_default() -> u64 {
    RETENTION_DEFAULT_DAYS
}

fn trash_root(disk: &Path) -> PathBuf {
    disk.join(MANIFEST_DIR).join(TRASH_DIR)
}

/// Marca de tiempo de alta resolución, de ancho FIJO:
/// `YYYYMMDD_HHMMSS_nnnnnnnnn` (nanosegundos).
/// Al ser de ancho fijo, el orden lexicográfico = orden cronológico,
/// lo que permite purgar comparando strings sin parser de fechas.
fn stamp_now() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}_{:09}", civil(now.as_secs()), now.subsec_nanos())
}

/// Convierte epoch secs a `YYYYMMDD_HHMMSS` (algoritmo days-from-civil de
/// Howard Hinnant; sin dependencias de fechas).
fn civil(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let s = secs % 86_400;
    let (h, mi, sec) = (s / 3600, (s % 3600) / 60, s % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if mo <= 2 { 1 } else { 0 };

    format!("{:04}{:02}{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, sec)
}

/// Borra `path` moviéndolo a la papelera del disco. `days == 0` = unlink.
/// Silencioso si el archivo ya no existe.
pub fn remove_or_trash(disk: &Path, path: &Path, days: u64) -> io::Result<()> {
    if days == 0 {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    if !path.exists() {
        return Ok(());
    }
    let rel = path
        .strip_prefix(disk)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path fuera del disco"))?;
    let dest_dir = trash_root(disk).join(stamp_now());
    let mut dest = dest_dir.join(rel);
    // Colisión: mismo archivo borrado dos veces en el mismo instante.
    let mut n = 1;
    while dest.exists() {
        let name = rel
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archivo".into());
        dest = dest_dir.join(rel.with_file_name(format!("{name}~{n}")));
        n += 1;
    }
    fs::create_dir_all(dest.parent().unwrap())
        .ctx(|| format!("creando papelera de {}", disk.display()))?;
    fs::rename(path, &dest)
        .ctx(|| format!("moviendo {} a la papelera", path.display()))?;
    Ok(())
}

/// Purga entradas de la papelera con más de `days` días. Devuelve cuántas
/// entradas (árboles completos por timestamp) eliminó.
pub fn purge(disk: &Path, days: u64) -> io::Result<usize> {
    let root = trash_root(disk);
    if days == 0 || !root.is_dir() {
        return Ok(0);
    }
    let cutoff_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(days * 86_400);
    let cutoff = civil(cutoff_secs);
    let stamp_len = stamp_now().len();

    let mut purged = 0;
    for e in fs::read_dir(&root)? {
        let e = e?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        // Solo tocar entradas con nuestro formato exacto (ancho fijo).
        if name.len() == stamp_len && name.as_ref() <= cutoff.as_str() {
            fs::remove_dir_all(e.path())?;
            purged += 1;
        }
    }
    Ok(purged)
}
