//! Batería 6: papelera de borrados (versioning ligero).
//!
//! INVARIANTE: la política sigue siendo "propagar borrados" (los discos
//! convergen), pero la ejecución mueve el archivo a
//! `.stalsync/trash/<timestamp>/<ruta>` en vez de unlink(). Es un rename
//! (mismo filesystem): instantáneo, cero bytes copiados, contenido intacto.

mod common;

use common::TestDisks;
use stalsync::sync::Options;
use stalsync::trash;
use std::fs;
use std::path::Path;

fn opts_con_papelera(days: u64) -> Options {
    Options {
        trash_days: days,
        ..Default::default()
    }
}

fn run(d: &TestDisks, days: u64) {
    stalsync::sync::run(&d.paths, &opts_con_papelera(days)).unwrap();
}

fn trash_entries(disk: &Path) -> Vec<std::path::PathBuf> {
    let root = disk.join(".stalsync/trash");
    let mut out = Vec::new();
    collect_files_rec(&root, &mut out);
    out
}

fn collect_files_rec(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect_files_rec(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn borrado_va_a_papelera_y_conserva_contenido() {
    // El archivo borrado sobrevive en la papelera del disco que lo perdió,
    // con su ruta original y contenido intacto — reversible.
    let d = TestDisks::new(2);
    d.write(0, "docs/a.txt", "contenido valioso");
    d.sync();
    d.remove(0, "docs/a.txt");

    run(&d, 30);

    assert!(!d.exists(0, "docs/a.txt"), "el archivo sale del árbol");
    assert!(!d.exists(1, "docs/a.txt"), "el borrado se propaga igual");
    let entries = trash_entries(&d.paths[1]);
    assert_eq!(entries.len(), 1, "la copia de d1 va a su papelera");
    assert_eq!(
        fs::read_to_string(&entries[0]).unwrap(),
        "contenido valioso"
    );
    assert!(
        entries[0].to_string_lossy().contains("docs"),
        "conserva la ruta relativa"
    );
    assert_eq!(
        trash_entries(&d.paths[0]).len(),
        0,
        "d0 ya no tenía el archivo: nada que trashear"
    );
}

#[test]
fn restaurar_desde_papelera_repropaga() {
    // FLUJO DE RESCATE: borrado accidental → el usuario saca el archivo de la
    // papelera → el siguiente sync lo repropaga a los demás discos.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();
    d.remove(0, "a.txt");
    run(&d, 30);

    // rescate: el archivo que la herramienta quitó de d1 está en la papelera
    // de d1; el usuario lo devuelve al árbol (de d0, donde lo había borrado)
    let entry = trash_entries(&d.paths[1]).pop().unwrap();
    fs::rename(&entry, d.paths[0].join("a.txt")).unwrap();

    run(&d, 30);

    assert!(d.exists(0, "a.txt"));
    assert!(d.exists(1, "a.txt"), "repropagado tras restaurar");
    assert_eq!(d.read(0, "a.txt"), "x");
}

#[test]
fn papelera_no_resucita_en_siguientes_syncs() {
    // La papelera vive bajo .stalsync/, que el escaneo excluye: los
    // archivos trashed no vuelven como "altas" ni cuentan como cambios.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();
    d.remove(0, "a.txt");
    run(&d, 30);

    // El siguiente sync no debe ver la papelera: 0 copias, convergido.
    let r = stalsync::status::compute_status(&d.paths).unwrap();
    assert!(r.convergido);
    assert_eq!(r.pendientes, 0);
}

#[test]
fn purga_elimina_entradas_viejas() {
    // Retención: purga() borra entradas con más de N días. Simulamos una
    // entrada vieja fabricando un timestamp del pasado (ancho fijo, orden
    // lexicográfico = cronológico).
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();
    d.remove(0, "a.txt");
    run(&d, 30);
    assert_eq!(trash_entries(&d.paths[1]).len(), 1);

    // Fabrica una entrada "vieja": copia el árbol de trash con timestamp antiguo.
    let trash = d.paths[1].join(".stalsync/trash");
    let vieja = trash.join("20200101_000000_000000000");
    fs::create_dir_all(&vieja).unwrap();
    fs::write(vieja.join("obsoleto.txt"), "basura vieja").unwrap();

    let purged = trash::purge(&d.paths[1], 30).unwrap();
    assert_eq!(purged, 1, "solo la entrada vieja se purga");
    assert_eq!(
        trash_entries(&d.paths[1]).len(),
        1,
        "la entrada reciente sobrevive"
    );
    assert!(!vieja.join("obsoleto.txt").exists());
}

#[test]
fn trash_days_cero_desactiva_papelera() {
    // --trash-days 0 = comportamiento clásico: unlink directo, sin papelera.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();
    d.remove(0, "a.txt");

    run(&d, 0);

    assert_eq!(trash_entries(&d.paths[0]).len(), 0, "sin papelera");
    assert!(!d.paths[0].join(".stalsync/trash").exists());
}

#[test]
fn dos_borrados_del_mismo_path_no_colapsan() {
    // Borrar el mismo archivo dos veces (con una edición de por medio)
    // conserva AMBAS versiones en la papelera, bajo timestamps distintos.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "v1");
    d.sync();
    d.remove(0, "a.txt");
    run(&d, 30);
    let primera = trash_entries(&d.paths[1]).pop().unwrap();
    // Copia (no mueve): la entrada T1 permanece en la papelera.
    fs::copy(&primera, d.paths[0].join("a.txt")).unwrap();
    d.write(0, "a.txt", "v2");
    d.sync(); // propaga v2 a d1
    d.remove(0, "a.txt");
    run(&d, 30);

    let entries = trash_entries(&d.paths[1]);
    assert_eq!(
        entries.len(),
        2,
        "dos versiones del mismo path en la papelera"
    );
    let contenidos: Vec<String> = entries
        .iter()
        .map(|e| fs::read_to_string(e).unwrap())
        .collect();
    assert!(contenidos.contains(&"v1".to_string()));
    assert!(contenidos.contains(&"v2".to_string()));
}
