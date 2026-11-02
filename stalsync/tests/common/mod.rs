//! Helpers compartidos por las baterías de tests.
//!
//! `allow(dead_code)`: cada batería usa solo el subconjunto de helpers que
//! necesita; sin esto clippy reporta métodos "sin usar" por crate de test.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// N discos temporales que se autoclean al terminar el test.
pub struct TestDisks {
    _temps: Vec<tempfile::TempDir>,
    pub paths: Vec<PathBuf>,
}

impl TestDisks {
    pub fn new(n: usize) -> Self {
        let temps: Vec<_> = (0..n).map(|_| tempfile::tempdir().unwrap()).collect();
        let paths = temps.iter().map(|t| t.path().to_path_buf()).collect();
        TestDisks {
            _temps: temps,
            paths,
        }
    }

    pub fn write(&self, i: usize, rel: &str, content: &str) {
        let p = self.paths[i].join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    pub fn read(&self, i: usize, rel: &str) -> String {
        fs::read_to_string(self.paths[i].join(rel)).unwrap()
    }

    pub fn exists(&self, i: usize, rel: &str) -> bool {
        self.paths[i].join(rel).exists()
    }

    pub fn remove(&self, i: usize, rel: &str) {
        fs::remove_file(self.paths[i].join(rel)).unwrap();
    }

    /// Fija el mtime en segundos desde epoch (para controlar quién "gana" un conflicto).
    pub fn set_mtime(&self, i: usize, rel: &str, secs: i64, nanos: u32) {
        filetime::set_file_mtime(
            self.paths[i].join(rel),
            filetime::FileTime::from_unix_time(secs, nanos),
        )
        .unwrap();
    }

    pub fn mtime_ns(&self, i: usize, rel: &str) -> i64 {
        let md = fs::metadata(self.paths[i].join(rel)).unwrap();
        let ft = filetime::FileTime::from_last_modification_time(&md);
        ft.unix_seconds() * 1_000_000_000 + ft.nanoseconds() as i64
    }

    /// Fija el mode POSIX (p. ej. 0o644, 0o755) de un archivo.
    #[cfg(unix)]
    pub fn set_mode(&self, i: usize, rel: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.paths[i].join(rel);
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(mode);
        fs::set_permissions(&p, perms).unwrap();
    }

    /// Mode POSIX actual de un archivo (los bits de permiso, 0o777).
    #[cfg(unix)]
    pub fn mode(&self, i: usize, rel: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(self.paths[i].join(rel))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    pub fn sync(&self) {
        stalsync::sync::run(
            &self.paths,
            &stalsync::sync::Options {
                dry_run: false,
                verbose: false,
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// Ejecuta un sync y devuelve (copias, borrados, conflictos) del plan.
    pub fn sync_stats(&self) -> (usize, usize, usize) {
        use stalsync::merge::{self, Action};
        let manifests: Vec<_> = self
            .paths
            .iter()
            .map(|p| stalsync::manifest::load(p).unwrap())
            .collect();
        let scans: Vec<_> = self
            .paths
            .iter()
            .zip(&manifests)
            .map(|(d, m)| stalsync::scan::scan_disk(d, m).unwrap())
            .collect();
        let r = merge::merge(&self.paths, &scans);
        let mut copies = 0;
        let mut deletes = 0;
        let mut conflicts = 0;
        for a in &r.actions {
            match a {
                Action::Copy { to, .. } => copies += to.len(),
                Action::Delete { .. } => deletes += 1,
                Action::RenameConflict { .. } => conflicts += 1,
                Action::Chmod { .. } => {}
            }
        }
        (copies, deletes, conflicts)
    }

    /// Hash agregado del árbol de archivos (excluye .stalsync/), para
    /// verificar que varios discos convergen al mismo contenido.
    pub fn tree_hash(&self, i: usize) -> String {
        tree_hash(&self.paths[i])
    }
}

pub fn tree_hash(root: &Path) -> String {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);
    files.sort();
    let mut hasher = blake3::Hasher::new();
    for rel in files {
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        hasher.update(&fs::read(root.join(&rel)).unwrap());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_name() == stalsync::manifest::MANIFEST_DIR {
            continue;
        }
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            collect_files(root, &e.path(), out);
        } else if ft.is_file() {
            out.push(e.path().strip_prefix(root).unwrap().to_path_buf());
        }
    }
}
