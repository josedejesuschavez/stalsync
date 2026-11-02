//! Escaneo de un disco con detección HASH_THEN_MTIME:
//! solo se hashea cuando size o mtime difieren del manifiesto.

use crate::manifest::FileEntry;
use crate::Ctx;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Igual (en contenido) a lo que dice su propio manifiesto.
    Clean(FileEntry),
    /// Contenido nuevo o distinto al de su manifiesto (o archivo nuevo).
    Changed(FileEntry),
    /// Estaba en el manifiesto pero ya no existe en disco.
    Deleted,
}

pub fn mtime_ns_of(md: &std::fs::Metadata) -> i64 {
    let ft = filetime::FileTime::from_last_modification_time(md);
    ft.unix_seconds() * 1_000_000_000 + ft.nanoseconds() as i64
}

pub(crate) fn hash_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn is_excluded(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().contains(".conflict-")
}

/// Escanea `disk` y devuelve el estado de cada ruta relativa conocida
/// (la unión de lo encontrado en disco y lo registrado en el manifiesto).
pub fn scan_disk(
    disk: &Path,
    manifest: &HashMap<PathBuf, FileEntry>,
) -> io::Result<HashMap<PathBuf, State>> {
    let mut states = HashMap::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let walker = WalkDir::new(disk)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name() != crate::manifest::MANIFEST_DIR);

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        if is_excluded(entry.file_name()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(disk)
            .expect("ruta bajo el root del disco")
            .to_path_buf();
        let md = entry.metadata()?;
        let size = md.len();
        let mtime_ns = mtime_ns_of(&md);

        let state = match manifest.get(&rel) {
            // size + mtime coinciden: sin cambios, reutilizamos el hash del manifiesto.
            Some(me) if me.size == size && me.mtime_ns == mtime_ns => State::Clean(FileEntry {
                size,
                mtime_ns,
                hash: me.hash.clone(),
            }),
            // Metadatos difieren: solo aquí pagamos el hash.
            Some(me) => {
                let hash = hash_file(entry.path())
                    .ctx(|| format!("hasheando {}", entry.path().display()))?;
                if hash == me.hash {
                    // Mismo contenido, solo cambió el mtime.
                    State::Clean(FileEntry { size, mtime_ns, hash })
                } else {
                    State::Changed(FileEntry { size, mtime_ns, hash })
                }
            }
            // No está en el manifiesto: archivo nuevo.
            None => State::Changed(FileEntry {
                size,
                mtime_ns,
                hash: hash_file(entry.path())
                    .ctx(|| format!("hasheando {}", entry.path().display()))?,
            }),
        };
        seen.insert(rel.clone());
        states.insert(rel, state);
    }

    // En el manifiesto pero no en disco: borrado.
    for path in manifest.keys() {
        if !seen.contains(path) {
            states.insert(path.clone(), State::Deleted);
        }
    }

    Ok(states)
}
