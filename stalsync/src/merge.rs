//! Merge N-way: combina los deltas de cada disco (medidos contra su propio
//! manifiesto) en un estado acordado y un plan de acciones.
//!
//! Estados por disco y ruta: Clean, Changed, Deleted o Absent (sin entrada).

use crate::manifest::FileEntry;
use crate::scan::State;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum Action {
    /// Copiar `path` desde el disco `from` hacia los discos `to`.
    Copy {
        path: PathBuf,
        from: usize,
        to: Vec<usize>,
    },
    /// Borrar `path` en los discos indicados.
    Delete { path: PathBuf, disks: Vec<usize> },
    /// Renombrar la versión perdedora de un conflicto en un disco.
    RenameConflict {
        disk: usize,
        from: PathBuf,
        to: PathBuf,
    },
    /// Aplicar permisos POSIX `mode` en los discos indicados, SIN tocar el
    /// contenido (propagación de un chmod; el contenido ya coincide).
    Chmod {
        path: PathBuf,
        mode: u32,
        disks: Vec<usize>,
    },
}

pub struct MergeResult {
    pub actions: Vec<Action>,
    /// Estado acordado final: lo que debe contener cada disco (y su manifiesto).
    pub agreed: HashMap<PathBuf, FileEntry>,
}

fn disk_stem(disk: &Path) -> String {
    disk.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("disco{}", 0))
}

fn conflict_name(path: &Path, disk: &Path, mtime_ns: i64) -> PathBuf {
    let stem = disk_stem(disk);
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivo".to_string());
    let renamed = format!("{file_name}.conflict-{stem}-{mtime_ns}");
    match path.parent() {
        Some(parent) if parent != Path::new("") => parent.join(renamed),
        _ => PathBuf::from(renamed),
    }
}

/// Emite Chmod para los discos presentes cuyo mode difiere del acordado.
/// `copied_to` se excluye: la copia ya aplica el mode al crear el archivo.
/// Solo actúa cuando el acordado tiene mode (nunca en Windows).
fn push_chmod(
    actions: &mut Vec<Action>,
    path: &Path,
    agreed: &FileEntry,
    present: &[(usize, &FileEntry, bool)],
    copied_to: &[usize],
) {
    let Some(mode) = agreed.mode else {
        return;
    };
    let targets: Vec<usize> = present
        .iter()
        .filter(|(i, e, _)| !copied_to.contains(i) && e.mode != Some(mode))
        .map(|(i, _, _)| *i)
        .collect();
    if !targets.is_empty() {
        actions.push(Action::Chmod {
            path: path.to_path_buf(),
            mode,
            disks: targets,
        });
    }
}

pub fn merge(disks: &[PathBuf], scans: &[HashMap<PathBuf, State>]) -> MergeResult {
    debug_assert_eq!(disks.len(), scans.len());
    let n = disks.len();

    // Unión de todas las rutas conocidas.
    let mut paths: HashSet<PathBuf> = HashSet::new();
    for scan in scans {
        paths.extend(scan.keys().cloned());
    }

    let mut actions = Vec::new();
    let mut agreed: HashMap<PathBuf, FileEntry> = HashMap::new();

    for path in paths {
        // Clasificación por disco.
        let mut present: Vec<(usize, &FileEntry, bool)> = Vec::new(); // (índice, entrada, changed)
        let mut deleted: Vec<usize> = Vec::new();
        for (i, scan) in scans.iter().enumerate() {
            match scan.get(&path) {
                Some(State::Clean(e)) | Some(State::Changed(e)) => {
                    present.push((i, e, matches!(scan.get(&path), Some(State::Changed(_)))));
                }
                Some(State::Deleted) => deleted.push(i),
                None => {} // Absent
            }
        }

        if present.is_empty() {
            // Borrado en todos los discos que lo tenían: nada que hacer.
            continue;
        }

        // Solo las entradas Changed compiten como versiones nuevas.
        // Una entrada Clean tiene el contenido viejo (línea base), no una modificación.
        let changed: Vec<(usize, &FileEntry)> = present
            .iter()
            .filter(|(_, _, c)| *c)
            .map(|(i, e, _)| (*i, *e))
            .collect();

        if changed.is_empty() {
            // Nadie modificó nada.
            if !deleted.is_empty() {
                // Hubo borrados → propagar el borrado a los que aún lo tienen.
                let victims: Vec<usize> = present.iter().map(|(i, _, _)| *i).collect();
                if !victims.is_empty() {
                    actions.push(Action::Delete {
                        path: path.clone(),
                        disks: victims,
                    });
                }
                // No entra al estado acordado.
            } else {
                // Nadie modificó ni borró: todos los present están Clean.
                // Pero los manifiestos pueden haber DIVERGIDO (un disco no
                // participó de syncs parciales del resto del grupo): dos
                // contenidos distintos sin que nadie "haya cambiado" nada.
                // No es conflicto de usuario: es desactualización. Gana el
                // contenido mayoritario (empate: mtime más reciente) y los
                // que divergen —y los ausentes— reciben la copia.
                let mut por_hash: HashMap<&str, Vec<(usize, &FileEntry)>> = HashMap::new();
                for (i, e, _) in &present {
                    por_hash.entry(e.hash.as_str()).or_default().push((*i, e));
                }
                let grupo_ganador = por_hash
                    .values()
                    .max_by_key(|g| (g.len(), g.iter().map(|(_, e)| e.mtime_ns).max()))
                    .expect("present no vacío");
                let agreed_entry = grupo_ganador
                    .iter()
                    .max_by_key(|(_, e)| e.mtime_ns)
                    .expect("grupo no vacío")
                    .1
                    .clone();
                let from = grupo_ganador[0].0;
                let to: Vec<usize> = (0..n)
                    .filter(|i| !grupo_ganador.iter().any(|(p, _)| p == i))
                    .collect();
                // Divergencia de manifiestos: también alinear permisos.
                // (Antes del Copy: `to` se mueve al pushear la acción.)
                push_chmod(&mut actions, &path, &agreed_entry, &present, &to);
                if !to.is_empty() {
                    actions.push(Action::Copy {
                        path: path.clone(),
                        from,
                        to,
                    });
                }
                agreed.insert(path.clone(), agreed_entry.clone());
            }
            continue;
        }

        // Hashes distintos entre las versiones modificadas.
        let distinct: HashSet<&str> = changed
            .iter()
            .map(|(_, e)| e.hash.as_str())
            .filter(|h| !h.is_empty())
            .collect();

        // Ganador: mtime más reciente entre los modificados.
        let (win_idx, win_entry) = changed
            .iter()
            .copied()
            .max_by_key(|(_, e)| e.mtime_ns)
            .expect("changed no vacío");

        // Un disco que ya tiene el hash ganador no recibe copia, aunque su
        // manifiesto esté viejo o perdido: el manifiesto es caché, la verdad
        // es el contenido. Perderlo cuesta rehash, no copias.
        let needs = |i: usize, present: &[(usize, &FileEntry, bool)]| -> bool {
            match present.iter().find(|(p, _, _)| *p == i) {
                Some((_, e, _)) => e.hash != win_entry.hash,
                None => true, // Deleted o Absent
            }
        };

        if distinct.len() <= 1 {
            // Consenso: una sola versión nueva.
            let mut to: Vec<usize> = Vec::new();
            to.extend(
                present
                    .iter()
                    .filter(|(i, _, _)| *i != win_idx && needs(*i, &present))
                    .map(|(i, _, _)| *i),
            );
            to.extend(deleted.iter().copied());
            to.extend(
                (0..n).filter(|i| !present.iter().any(|(p, _, _)| p == i) && !deleted.contains(i)),
            );
            // La copia aplica el mode al crear; los que ya tenían el
            // contenido reciben solo el chmod. (Antes del Copy: `to` se
            // mueve al pushear la acción.)
            push_chmod(&mut actions, &path, win_entry, &present, &to);
            if !to.is_empty() {
                actions.push(Action::Copy {
                    path: path.clone(),
                    from: win_idx,
                    to,
                });
            }
            agreed.insert(path.clone(), win_entry.clone());
        } else {
            // Conflicto real: versiones distintas en discos modificados.
            let mut to: Vec<usize> = Vec::new();
            for (i, e) in &changed {
                if *i != win_idx && e.hash != win_entry.hash {
                    // Perdedor: conservar su versión renombrada y recibir la ganadora.
                    actions.push(Action::RenameConflict {
                        disk: *i,
                        from: path.clone(),
                        to: conflict_name(&path, &disks[*i], e.mtime_ns),
                    });
                    to.push(*i);
                }
            }
            to.extend(
                present
                    .iter()
                    .filter(|(i, _, _)| {
                        *i != win_idx && !changed.iter().any(|(c, _)| c == i) && needs(*i, &present)
                    })
                    .map(|(i, _, _)| *i),
            );
            to.extend(deleted.iter().copied());
            to.extend((0..n).filter(|i| {
                *i != win_idx && !present.iter().any(|(p, _, _)| p == i) && !deleted.contains(i)
            }));

            push_chmod(&mut actions, &path, win_entry, &present, &to);
            if !to.is_empty() {
                actions.push(Action::Copy {
                    path: path.clone(),
                    from: win_idx,
                    to,
                });
            }
            agreed.insert(path.clone(), win_entry.clone());
        }
    }

    MergeResult { actions, agreed }
}
