//! Batería de permisos POSIX (mode): preservación al copiar y propagación
//! de un chmod sin recopiar contenido. Todo el archivo es cfg(unix): en
//! Windows no hay modes POSIX y stalsync lo trata como no-op.

#![cfg(unix)]

mod common;

use common::TestDisks;

#[test]
fn permisos_se_preservan_al_copiar() {
    // PROMESA: un archivo copiado llega con su mode al disco destino
    // (un script 0o751 no pierde el +x al sincronizarse).
    let d = TestDisks::new(2);
    d.write(0, "script.sh", "#!/bin/sh\necho ok\n");
    d.set_mode(0, "script.sh", 0o751);

    d.sync();

    assert_eq!(d.mode(1, "script.sh"), 0o751);
}

#[test]
fn chmod_se_propaga_sin_recopiar_contenido() {
    // REGLA: un cambio de permisos es un cambio más: se propaga con un
    // chmod barato, NO recopiando el archivo (el hash no cambió).
    let d = TestDisks::new(2);
    d.write(0, "privado.txt", "dato");
    d.sync();

    d.set_mode(0, "privado.txt", 0o600);
    let (copias, ..) = d.sync_stats();
    assert_eq!(copias, 0, "chmod no debe generar copias");

    d.sync();
    assert_eq!(d.mode(1, "privado.txt"), 0o600);
}

#[test]
fn chmod_converge_en_ambas_direcciones() {
    // Si el destino tiene el mode desactualizado, un sync lo alinea aunque
    // nadie haya tocado el archivo desde el último manifiesto (divergencia
    // de manifiestos, misma regla que el contenido).
    let d = TestDisks::new(2);
    d.write(0, "f.txt", "x");
    d.sync();

    // Ajuste manual directo en disco, sin pasar por stalsync: el manifiesto
    // de d1 sigue diciendo 0o644 (o lo que creara tempfile).
    d.set_mode(1, "f.txt", 0o640);

    d.sync();
    assert_eq!(d.mode(1, "f.txt"), d.mode(0, "f.txt"));
}

#[test]
fn no_perms_no_aplica_mode() {
    // --no-perms (preserve_perms = false): comportamiento v0.1.0, el
    // destino queda con los permisos por defecto de creación.
    let d = TestDisks::new(2);
    d.write(0, "f.txt", "x");
    d.set_mode(0, "f.txt", 0o700);

    stalsync::sync::run(
        &d.paths,
        &stalsync::sync::Options {
            preserve_perms: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert_ne!(d.mode(1, "f.txt"), 0o700, "no-perms no debe aplicar mode");
}

#[test]
fn mode_vive_en_el_manifiesto() {
    // El mode queda registrado en el manifiesto (campo `mode`): es parte
    // del estado acordado, no algo que se relee del disco origen en cada
    // pasada.
    let d = TestDisks::new(2);
    d.write(0, "f.txt", "x");
    d.set_mode(0, "f.txt", 0o750);
    d.sync();

    let m = stalsync::manifest::load(&d.paths[0]).unwrap();
    assert_eq!(
        m[std::path::Path::new("f.txt")].mode.map(|m| m & 0o777),
        Some(0o750),
        "el manifiesto guarda el mode del disco"
    );
}
