//! Batería 4: el comando `status` (solo lectura).
//!
//! INVARIANTE: status reporta EXACTAMENTE lo que un sync haría (mismo scan,
//! mismo merge), sin tocar nada. Si status dice "todo sincronizado", un sync
//! no copia nada; si dice "N transferencias pendientes", el sync las ejecuta.

mod common;

use common::TestDisks;
use stalsync::status::{compute_status, render};

fn status(d: &TestDisks) -> stalsync::status::StatusReport {
    compute_status(&d.paths).unwrap()
}

#[test]
fn convergido_despues_de_sync() {
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "x");
    d.write(0, "sub/b.txt", "y");
    d.sync();

    let r = status(&d);
    assert!(r.convergido);
    assert_eq!(r.pendientes, 0);
    assert_eq!(r.rutas, 2);
    assert!(r.disks.iter().all(|x| !x.desfasado()));
}

#[test]
fn detecta_disco_atrasado() {
    // hdd1 tiene contenido que hdd2/hdd3 aún no recibieron: status debe
    // reportar que hdd2 y hdd3 RECIBEN, sin que nadie tenga "cambios locales"
    // (nadie editó nada; están desactualizados respecto al acuerdo).
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "x");
    d.sync();
    // simula disco atrasado: borra el contenido de d2 y su manifiesto
    std::fs::remove_dir_all(&d.paths[1]).unwrap();
    std::fs::create_dir(&d.paths[1]).unwrap();

    let r = status(&d);
    assert!(!r.convergido);
    assert_eq!(r.disks[1].recibe, 1);
    assert_eq!(r.disks[1].cambios, 0, "recibir no es un cambio local");
    assert!(r.disks[1].desfasado());
    assert!(!r.disks[0].desfasado());
}

#[test]
fn detecta_cambios_y_borrados_locales() {
    // Cambios locales: modificaciones/altas/bajas medidas contra el propio
    // manifiesto de cada disco, atribuidos al disco que los hizo.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "x");
    d.write(0, "b.txt", "y");
    d.write(0, "c.txt", "z");
    d.sync();

    d.write(1, "a.txt", "editado"); // d1 modifica
    d.write(1, "nuevo.txt", "n"); // d1 crea
    d.remove(2, "b.txt"); // d2 borra

    let r = status(&d);
    assert!(!r.convergido);
    assert_eq!(r.disks[1].cambios, 2);
    assert_eq!(r.disks[1].borrados, 0);
    assert_eq!(r.disks[2].borrados, 1);
    assert_eq!(r.disks[2].cambios, 0);
    // d1 envía 2; d0 y d2 reciben la edición y el nuevo
    assert_eq!(r.disks[1].envia, 2);
    assert!(r.disks[0].recibe >= 2);
    // d0 y d1 perderán b.txt por el borrado de d2
    assert_eq!(r.disks[0].perdera, 1);
    assert_eq!(r.disks[1].perdera, 1);
}

#[test]
fn detecta_conflictos() {
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "base");
    d.sync();

    d.write(0, "a.txt", "v1");
    d.set_mtime(0, "a.txt", 1_700_000_000, 0);
    d.write(1, "a.txt", "v2");
    d.set_mtime(1, "a.txt", 1_700_100_000, 0);

    let r = status(&d);
    assert!(!r.convergido);
    assert_eq!(r.disks[0].conflictos, 1, "d0 es el perdedor");
    assert_eq!(r.disks[1].conflictos, 0, "d1 gana");
}

#[test]
fn render_convergido_y_desfasado() {
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();

    let ok = render(&status(&d));
    assert!(ok.contains("✔ todo sincronizado"), "{ok}");

    d.write(0, "b.txt", "pendiente");
    let ko = render(&status(&d));
    assert!(ko.contains("desfasado"), "{ko}");
    assert!(ko.contains("recibe"), "{ko}");
    assert!(ko.contains("disco"), "{ko}");
}

#[test]
fn status_no_toca_nada() {
    // INVARIANTE duro: ejecutar status es puramente diagnóstico. Correrlo
    // dos veces sobre estado con desfase debe dar el MISMO reporte (no
    // escribe manifiestos, no copia, no borra).
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();
    d.write(0, "b.txt", "nuevo");

    let r1 = status(&d);
    let r2 = status(&d);
    assert_eq!(r1.pendientes, r2.pendientes);
    assert_eq!(r1.disks[1].recibe, r2.disks[1].recibe);
    // El archivo sigue solo en d0: status no lo propagó
    assert!(d.exists(0, "b.txt"));
    assert!(!d.exists(1, "b.txt"));
}
