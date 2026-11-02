//! Manifiesto por disco: último estado acordado (caché, no verdad).
//! Formato: JSON Lines en <disco>/.stalsync/manifest.jsonl
//! Escritura atómica: tmp + fsync + rename.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::Ctx;

pub const MANIFEST_DIR: &str = ".stalsync";
pub const MANIFEST_FILE: &str = "manifest.jsonl";

/// Versión del formato del manifiesto. Subirla cuando cambie el formato;
/// load rechaza versiones mayores con error claro en vez de interpretar
/// basura. Los manifiestos sin header se asumen v1 (formato original).
const FORMAT_VERSION: u64 = 1;

/// Header opcional de la primera línea: declara la versión del formato.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Header {
    format: u64,
}

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
    let mut first = true;
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line.ctx(|| format!("leyendo {} línea {}", path.display(), n + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        // Primera línea no vacía: si es un header, validar la versión del
        // formato. Si no lo es, es un manifiesto heredado (v1 implícito)
        // y la línea se procesa como entrada normal.
        if first {
            first = false;
            if let Ok(h) = serde_json::from_str::<Header>(&line) {
                if h.format > FORMAT_VERSION {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "manifiesto {} con formato v{} no soportado \
                             (esta versión de stalsync entiende hasta v{FORMAT_VERSION}); \
                             actualiza stalsync",
                            path.display(),
                            h.format
                        ),
                    ));
                }
                continue;
            }
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
        let mut w = BufWriter::new(
            File::create(&tmp)
                .ctx(|| format!("creando manifiesto temporal en {}", dir.display()))?,
        );
        // Header de versión en la primera línea. Un lector antiguo (sin
        // esta lógica) lo ignora con warning: la línea no parsea como
        // entrada, comportamiento tolerante heredado de load.
        serde_json::to_writer(&mut w, &Header { format: FORMAT_VERSION })?;
        w.write_all(b"\n")?;
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
        w.get_ref()
            .sync_all()
            .ctx(|| format!("fsync del manifiesto temporal {}", tmp.display()))?;
    }
    fs::rename(&tmp, manifest_path(disk))
        .ctx(|| format!("publicando manifiesto de {}", disk.display()))?;
    Ok(())
}
