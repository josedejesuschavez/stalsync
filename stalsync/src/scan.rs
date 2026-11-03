//! Escaneo de un disco con detección HASH_THEN_MTIME:
//! solo se hashea cuando size o mtime difieren del manifiesto.

use crate::Ctx;
use crate::manifest::FileEntry;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Igual (en contenido) a lo que dice su propio manifiesto.
    Clean(FileEntry),
    /// Contenido nuevo/distinto al de su manifiesto (o archivo nuevo),
    /// o mismo contenido con permisos (mode) distintos: un chmod también
    /// es un cambio a propagar.
    Changed(FileEntry),
    /// Estaba en el manifiesto pero ya no existe en disco.
    Deleted,
}

pub fn mtime_ns_of(md: &std::fs::Metadata) -> i64 {
    let ft = filetime::FileTime::from_last_modification_time(md);
    ft.unix_seconds() * 1_000_000_000 + ft.nanoseconds() as i64
}

/// Permisos POSIX del archivo, si la plataforma los tiene. En Windows
/// devuelve None: los manifiestos son intercambiables entre SO y los modes
/// solo se propagan donde existen.
#[cfg(unix)]
pub(crate) fn mode_of(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode())
}

#[cfg(not(unix))]
pub(crate) fn mode_of(_md: &std::fs::Metadata) -> Option<u32> {
    None
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
    scan_disk_with_progress(disk, manifest, |_, _| {})
}

/// Igual que `scan_disk`, pero invoca `progress(n_archivos, bytes)` por cada
/// archivo procesado. Sirve para mostrar avance en el spinner: el primer
/// sync de un disco grande hashea todo el árbol y sin contador parece
/// colgado; los bytes dan idea del progreso real con archivos grandes.
pub fn scan_disk_with_progress(
    disk: &Path,
    manifest: &HashMap<PathBuf, FileEntry>,
    mut progress: impl FnMut(usize, u64),
) -> io::Result<HashMap<PathBuf, State>> {
    let mut states = HashMap::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut n_files = 0usize;
    let mut n_bytes = 0u64;

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
        let mode = mode_of(&md);

        let state = match manifest.get(&rel) {
            // size + mtime + mode coinciden: sin cambios, reutilizamos el
            // hash del manifiesto.
            Some(me) if me.size == size && me.mtime_ns == mtime_ns && me.mode == mode => {
                State::Clean(FileEntry {
                    size,
                    mtime_ns,
                    hash: me.hash.clone(),
                    mode,
                })
            }
            // size + mtime coinciden pero el mode difiere: cambió SOLO el
            // chmod. Se reporta como Changed para que el merge propague el
            // permiso (sin recopiar: el contenido está garantizado por
            // size+mtime y el hash se reutiliza del manifiesto).
            Some(me) if me.size == size && me.mtime_ns == mtime_ns => State::Changed(FileEntry {
                size,
                mtime_ns,
                hash: me.hash.clone(),
                mode,
            }),
            // Metadatos difieren: solo aquí pagamos el hash.
            Some(me) => {
                let hash = hash_file(entry.path())
                    .ctx(|| format!("hasheando {}", entry.path().display()))?;
                if hash == me.hash {
                    // Mismo contenido, solo cambió el mtime.
                    State::Clean(FileEntry {
                        size,
                        mtime_ns,
                        hash,
                        mode,
                    })
                } else {
                    State::Changed(FileEntry {
                        size,
                        mtime_ns,
                        hash,
                        mode,
                    })
                }
            }
            // No está en el manifiesto: archivo nuevo.
            None => State::Changed(FileEntry {
                size,
                mtime_ns,
                hash: hash_file(entry.path())
                    .ctx(|| format!("hasheando {}", entry.path().display()))?,
                mode,
            }),
        };
        seen.insert(rel.clone());
        states.insert(rel, state);
        n_files += 1;
        n_bytes += size;
        progress(n_files, n_bytes);
    }

    // En el manifiesto pero no en disco: borrado.
    for path in manifest.keys() {
        if !seen.contains(path) {
            states.insert(path.clone(), State::Deleted);
        }
    }

    Ok(states)
}
