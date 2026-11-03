//! Tests del progreso de escaneo: el callback reporta un conteo por archivo,
//! para que el primer sync de un disco grande no parezca colgado.

mod common;

use common::TestDisks;

#[test]
fn progreso_reporta_cada_archivo() {
    let d = TestDisks::new(1);
    for i in 0..10 {
        d.write(0, &format!("f{i}.txt"), "x");
    }
    let manifest = stalsync::manifest::load(&d.paths[0]).unwrap();

    let mut ultimo = 0;
    stalsync::scan::scan_disk_with_progress(&d.paths[0], &manifest, |n| ultimo = n).unwrap();
    assert_eq!(ultimo, 10, "el conteo final debe ser el número de archivos");
}

#[test]
fn progreso_va_en_orden_ascendente() {
    let d = TestDisks::new(1);
    d.write(0, "a.txt", "a");
    d.write(0, "sub/b.txt", "b");
    let manifest = stalsync::manifest::load(&d.paths[0]).unwrap();

    let mut visto = Vec::new();
    stalsync::scan::scan_disk_with_progress(&d.paths[0], &manifest, |n| visto.push(n)).unwrap();
    let mut ordenado = visto.clone();
    ordenado.sort();
    assert_eq!(visto, ordenado, "los conteos deben ser 1,2,3... en orden");
}

#[test]
fn scan_disk_clasico_sigue_igual() {
    // Compatibilidad: scan_disk (sin callback) produce el mismo resultado.
    let d = TestDisks::new(1);
    d.write(0, "a.txt", "a");
    let manifest = stalsync::manifest::load(&d.paths[0]).unwrap();

    let con = stalsync::scan::scan_disk_with_progress(&d.paths[0], &manifest, |_| {}).unwrap();
    let sin = stalsync::scan::scan_disk(&d.paths[0], &manifest).unwrap();
    assert_eq!(con, sin);
}
