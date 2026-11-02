//! Manifiesto por disco: último estado acordado (caché, no verdad).
//! Formato: JSON Lines en <disco>/.stalsync/manifest.jsonl
//! Escritura atómica: tmp + fsync + rename.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

pub const MANIFEST_DIR: &str = ".stalsync";
pub const MANIFEST_FILE: &str = "manifest.jsonl";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub size: u64,
    pub mtime_ns: i64,
    /// BLAKE3 en hex. Vacío = desconocido (se recalcula al comparar).
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Line {
    path: PathBuf,
    size: u64,
    mtime_ns: i64,
    hash: String,
}

pub fn manifest_path(disk: &Path) -> PathBuf {
    disk.join(MANIFEST_DIR).join(MANIFEST_FILE)
}

/// Carga el manifiesto. Si no existe, devuelve mapa vacío.
/// Líneas malformadas se ignoran con advertencia (es caché, no verdad).
pub fn load(disk: &Path) -> std::io::Result<HashMap<PathBuf, FileEntry>> {
    let mut map = HashMap::new();
    let path = manifest_path(disk);
    let Ok(file) = File::open(&path) else {
        return Ok(map);
    };
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Line>(&line) {
            Ok(l) => {
                map.insert(
                    l.path,
                    FileEntry {
                        size: l.size,
                        mtime_ns: l.mtime_ns,
                        hash: l.hash,
                    },
                );
            }
            Err(e) => eprintln!("advertencia: manifiesto {} línea {} inválida: {e}", path.display(), n + 1),
        }
    }
    Ok(map)
}

/// Escribe el manifiesto de forma atómica: tmp + fsync + rename.
pub fn save(disk: &Path, entries: &HashMap<PathBuf, FileEntry>) -> std::io::Result<()> {
    let dir = disk.join(MANIFEST_DIR);
    fs::create_dir_all(&dir)?;
    let tmp = dir.join("manifest.jsonl.tmp");
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        for (path, e) in entries {
            let line = Line {
                path: path.clone(),
                size: e.size,
                mtime_ns: e.mtime_ns,
                hash: e.hash.clone(),
            };
            serde_json::to_writer(&mut w, &line)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    fs::rename(&tmp, manifest_path(disk))?;
    Ok(())
}
