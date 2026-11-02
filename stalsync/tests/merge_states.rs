//! Batería 1: la tabla de estados del merge.
//!
//! Cada test protege una fila de las reglas de merge y documenta POR QUÉ existe.
//! Estados por disco: Clean (igual a su manifiesto), Changed (contenido nuevo),
//! Deleted (estaba, ya no), Absent (sin entrada en el mapa).

use stalsync::manifest::FileEntry;
use stalsync::merge::{Action, merge};
use stalsync::scan::State;
use std::collections::HashMap;
use std::path::PathBuf;

fn entry(size: u64, mtime_ns: i64, tag: &str) -> FileEntry {
    FileEntry {
        size,
        mtime_ns,
        hash: blake3::hash(tag.as_bytes()).to_hex().to_string(),
        // Los tests de merge no modelan permisos: mode desconocido.
        mode: None,
    }
}

fn changed(size: u64, mtime_ns: i64, tag: &str) -> State {
    State::Changed(entry(size, mtime_ns, tag))
}

fn clean(size: u64, mtime_ns: i64, tag: &str) -> State {
    State::Clean(entry(size, mtime_ns, tag))
}

fn one(tag: &str) -> State {
    changed(tag.len() as u64, 1_000, tag)
}

fn scans(states: Vec<Vec<(&str, State)>>) -> (Vec<PathBuf>, Vec<HashMap<PathBuf, State>>) {
    let disks: Vec<PathBuf> = (0..states.len())
        .map(|i| PathBuf::from(format!("/d{i}")))
        .collect();
    let maps = states
        .into_iter()
        .map(|v| v.into_iter().map(|(p, s)| (PathBuf::from(p), s)).collect())
        .collect();
    (disks, maps)
}

fn actions(r: &stalsync::merge::MergeResult) -> &[Action] {
    &r.actions
}

#[test]
fn unico_changed_propaga_a_ausentes() {
    // REGLA: una sola modificación es el consenso; los discos que no tienen
    // el archivo (Absent, p. ej. un disco nuevo) lo reciben.
    let (disks, s) = scans(vec![vec![("a.txt", one("nuevo"))], vec![], vec![]]);
    let r = merge(&disks, &s);
    assert!(matches!(
        actions(&r),
        [Action::Copy { path, from: 0, to, .. }] if path == &PathBuf::from("a.txt") && to == &vec![1, 2]
    ));
    assert!(r.agreed.contains_key(&PathBuf::from("a.txt")));
}

#[test]
fn cambio_identico_en_dos_discos_propaga_al_tercero() {
    // REGLA: dos discos pueden llegar al mismo contenido por caminos distintos
    // (mismo hash). Es consenso, no conflicto: el tercero recibe.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(4, 100, "mismo"))],
        vec![("a.txt", changed(4, 200, "mismo"))],
        vec![],
    ]);
    let r = merge(&disks, &s);
    assert_eq!(actions(&r).len(), 1);
    assert!(matches!(
        actions(&r),
        [Action::Copy { from, to, .. }] if (*from == 0 || *from == 1) && to == &vec![2]
    ));
    assert_eq!(
        actions(&r)
            .iter()
            .filter(|a| matches!(a, Action::RenameConflict { .. }))
            .count(),
        0
    );
}

#[test]
fn borrado_puro_se_propaga() {
    // REGLA: si nadie modificó el archivo y alguien lo borró, el borrado es
    // la intención del usuario y se propaga. El archivo sale del estado acordado.
    let (disks, s) = scans(vec![
        vec![("a.txt", clean(4, 100, "viejo"))],
        vec![("a.txt", State::Deleted)],
        vec![("a.txt", clean(4, 100, "viejo"))],
    ]);
    let r = merge(&disks, &s);
    assert!(matches!(
        actions(&r),
        [Action::Delete { path, disks: d }] if path == &PathBuf::from("a.txt") && d == &vec![0, 2]
    ));
    assert!(!r.agreed.contains_key(&PathBuf::from("a.txt")));
}

#[test]
fn borrado_en_todos_no_genera_acciones() {
    // REGLA: si todos los que lo tenían lo borraron, no hay nada que hacer.
    let (disks, s) = scans(vec![
        vec![("a.txt", State::Deleted)],
        vec![("a.txt", State::Deleted)],
        vec![],
    ]);
    let r = merge(&disks, &s);
    assert!(actions(&r).is_empty());
    assert!(!r.agreed.contains_key(&PathBuf::from("a.txt")));
}

#[test]
fn modificacion_resucita_borrado() {
    // REGLA (política conservadora): modificación vs borrado gana la modificación.
    // Borrar puede ser un accidente; perder una edición no debe serlo.
    // El disco que borró recibe el contenido de vuelta.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(7, 200, "editado"))],
        vec![("a.txt", State::Deleted)],
        vec![("a.txt", clean(4, 100, "viejo"))],
    ]);
    let r = merge(&disks, &s);
    assert!(matches!(
        actions(&r),
        [Action::Copy { from: 0, to, .. }] if to.len() == 2 && to.contains(&1) && to.contains(&2)
    ));
    assert_eq!(
        r.agreed[&PathBuf::from("a.txt")].hash,
        blake3::hash(b"editado").to_hex().to_string()
    );
}

#[test]
fn conflictos_gana_mtime_mas_reciente_y_perdedor_renombrado() {
    // REGLA: dos modificaciones distintas son un conflicto real. Gana el mtime
    // más reciente. El perdedor NO se pierde: se renombra a *.conflict-*
    // SOLO en su disco, y recibe el contenido ganador.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(5, 100, "viejo"))],
        vec![("a.txt", changed(6, 300, "GANADOR"))],
        vec![("a.txt", clean(4, 50, "base"))],
    ]);
    let r = merge(&disks, &s);
    assert!(actions(&r).iter().any(|a| matches!(
        a,
        Action::RenameConflict { disk: 0, to, .. } if to.to_string_lossy().contains(".conflict-d0-")
    )));
    assert!(actions(&r).iter().any(|a| matches!(
        a,
        Action::Copy { from: 1, to, .. } if to.contains(&0) && to.contains(&2)
    )));
    assert_eq!(
        r.agreed[&PathBuf::from("a.txt")].hash,
        blake3::hash(b"GANADOR").to_hex().to_string()
    );
}

#[test]
fn conflicto_entre_tres_versiones() {
    // REGLA: con 3 versiones distintas, una sola gana (mtime más reciente);
    // los otros dos discos conservan su versión renombrada.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(1, 100, "v1"))],
        vec![("a.txt", changed(1, 900, "v3-gana"))],
        vec![("a.txt", changed(1, 500, "v2"))],
    ]);
    let r = merge(&disks, &s);
    let renames = actions(&r)
        .iter()
        .filter(|a| matches!(a, Action::RenameConflict { .. }))
        .count();
    assert_eq!(renames, 2, "los dos perdedores conservan su versión");
    assert_eq!(
        r.agreed[&PathBuf::from("a.txt")].hash,
        blake3::hash(b"v3-gana").to_hex().to_string()
    );
}

// ---------------------------------------------------------------------------
// REGRESIÓN de bug real encontrado en las pruebas manuales:
// el merge comparaba hashes entre TODOS los presentes, así un disco Clean
// (que tiene el contenido VIEJO de la línea base) "competía" contra el
// Changed y cada modificación legítima se reportaba como conflicto.
// ---------------------------------------------------------------------------

#[test]
fn clean_no_compite_contra_changed() {
    // Una modificación legítima en un disco, con los demás en la línea base
    // (Clean), NO es un conflicto: es propagación simple.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(6, 200, "nuevo"))],
        vec![("a.txt", clean(4, 100, "base"))],
        vec![("a.txt", clean(4, 100, "base"))],
    ]);
    let r = merge(&disks, &s);
    assert_eq!(
        actions(&r)
            .iter()
            .filter(|a| matches!(a, Action::RenameConflict { .. }))
            .count(),
        0
    );
    assert!(actions(&r).iter().any(|a| matches!(
        a,
        Action::Copy { from: 0, to, .. } if to.len() == 2 && to.contains(&1) && to.contains(&2)
    )));
}

// ---------------------------------------------------------------------------
// El manifiesto es caché: si un disco quedó con manifiesto viejo/perdido pero
// su CONTENIDO ya es el acordado, no recibe copia. Perder un manifiesto cuesta
// rehash, no bytes copiados.
// ---------------------------------------------------------------------------

#[test]
fn contenido_identico_no_recibe_copia_aunque_manifiesto_este_viejo() {
    // d2 tiene el contenido nuevo pero figura como Changed (manifiesto viejo).
    // Su hash ya es el acordado → no debe estar en los destinos de la copia.
    let (disks, s) = scans(vec![
        vec![("a.txt", changed(6, 200, "nuevo"))],
        vec![("a.txt", changed(6, 999, "nuevo"))], // mismo contenido, mtime distinto
        vec![("a.txt", clean(4, 100, "base"))],
    ]);
    let r = merge(&disks, &s);
    assert!(actions(&r).iter().all(|a| match a {
        Action::Copy { to, .. } => !to.contains(&1),
        _ => true,
    }));
    assert!(actions(&r).iter().any(|a| matches!(
        a,
        Action::Copy { to, .. } if to == &vec![2]
    )));
}

#[test]
fn disco_ausente_del_grupo_recibe_todo_sin_cambios() {
    // Un disco que no participó del último acuerdo (Absent en todo) recibe
    // el estado acordado completo, aunque nadie haya "cambiado" nada.
    let (disks, s) = scans(vec![
        vec![("a.txt", clean(4, 100, "base"))],
        vec![("a.txt", clean(4, 100, "base"))],
        vec![],
    ]);
    let r = merge(&disks, &s);
    assert!(matches!(
        actions(&r),
        [Action::Copy { from: 0, to, .. }] if to == &vec![2]
    ));
}
