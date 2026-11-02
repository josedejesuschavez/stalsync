//! Batería 2: end-to-end con discos temporales reales.
//!
//! Estos tests ejercitan el camino completo scan → merge → copy → manifest,
//! y fijan el comportamiento observable que el diseño promete.

mod common;

use common::TestDisks;

fn tres() -> TestDisks {
    TestDisks::new(3)
}

#[test]
fn primer_sync_replica_todo_y_converge() {
    // PROMESA: al final de una pasada, todos los discos tienen el mismo
    // contenido (hash agregado idéntico), con mtime preservado.
    let d = tres();
    d.write(0, "a.txt", "contenido A");
    d.write(0, "sub/b.txt", "contenido B");
    // nanos = 123_456_700 (múltiplo de 100): NTFS redondea mtimes a
    // intervalos de 100ns (FILETIME); el test debe usar un mtime
    // representable en todos los filesystems soportados. La tolerancia al
    // redondeo la cubre la detección HASH_THEN_MTIME (hash = verdad).
    d.set_mtime(0, "a.txt", 1_700_000_000, 123_456_700);

    d.sync();

    for i in [1, 2] {
        assert_eq!(d.read(i, "a.txt"), "contenido A");
        assert_eq!(d.read(i, "sub/b.txt"), "contenido B");
        assert_eq!(
            d.mtime_ns(i, "a.txt"),
            1_700_000_000_000_000_000 + 123_456_700
        );
    }
    let h = d.tree_hash(0);
    assert_eq!(h, d.tree_hash(1));
    assert_eq!(h, d.tree_hash(2));
}

#[test]
fn re_sync_sin_cambios_no_copia_nada() {
    // PROMESA (HASH_THEN_MTIME): si size+mtime coinciden con el manifiesto,
    // no se hashea ni se copia. El re-sync típico cuesta recorrer + stat.
    let d = tres();
    d.write(0, "a.txt", "dato");
    d.sync();
    assert_eq!(d.sync_stats(), (0, 0, 0));
}

#[test]
fn modificacion_en_cualquier_disco_propaga_a_los_demas() {
    // PROMESA (peer-to-peer): no hay maestro. Cambiar en d2 propaga a d1 y d3.
    let d = tres();
    d.write(0, "a.txt", "original");
    d.sync();

    d.write(1, "a.txt", "editado en d2");
    d.sync();

    assert_eq!(d.read(0, "a.txt"), "editado en d2");
    assert_eq!(d.read(2, "a.txt"), "editado en d2");
}

#[test]
fn borrado_en_cualquier_disco_propaga_a_los_demas() {
    // PROMESA: un borrado sin modificaciones concurrentes es una intención
    // del usuario y se propaga. El archivo desaparece de todos los discos.
    let d = tres();
    d.write(0, "a.txt", "x");
    d.write(0, "b.txt", "y");
    d.sync();

    d.remove(2, "a.txt");
    d.sync();

    for i in [0, 1, 2] {
        assert!(!d.exists(i, "a.txt"), "a.txt debió borrarse en d{i}");
    }
    assert!(d.exists(1, "b.txt"), "b.txt no debe tocarse");
}

#[test]
fn solo_cambio_de_mtime_no_genera_copia() {
    // PROMESA (el hash es la verdad, el mtime es solo optimización):
    // tocar el mtime sin cambiar el contenido no propaga nada; solo se
    // actualiza el manifiesto local.
    let d = tres();
    d.write(0, "a.txt", "contenido fijo");
    d.sync();

    d.set_mtime(1, "a.txt", 1_750_000_000, 0);
    let (copias, borrados, conflictos) = d.sync_stats();
    assert_eq!((copias, borrados, conflictos), (0, 0, 0));
}

#[test]
fn conflicto_end_to_end_gana_mtime_y_conserva_perdedor() {
    // PROMESA (política de conflictos): gana el mtime más reciente en todos
    // los discos; la versión perdedora se conserva renombrada SOLO en su
    // disco y NO se propaga ni entra al manifiesto.
    let d = tres();
    d.write(0, "a.txt", "base");
    d.sync();

    d.write(0, "a.txt", "versión de d1");
    d.set_mtime(0, "a.txt", 1_700_000_000, 0);
    d.write(1, "a.txt", "versión de d2 GANADORA");
    d.set_mtime(1, "a.txt", 1_700_100_000, 0);

    let (.., conflictos) = d.sync_stats();
    assert_eq!(conflictos, 1);
    d.sync();

    for i in [0, 1, 2] {
        assert_eq!(d.read(i, "a.txt"), "versión de d2 GANADORA");
    }
    // Perdedor conservado solo en d1, con sufijo .conflict-<nombre_del_disco>-
    let conflict = std::fs::read_dir(&d.paths[0])
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .find(|n| n.contains(".conflict-"))
        .expect("d1 conserva su versión como conflict file");
    assert_eq!(
        std::fs::read_to_string(d.paths[0].join(&conflict)).unwrap(),
        "versión de d1"
    );
    // Ni en d2 ni en d3, ni en el manifiesto de d1.
    for i in [1, 2] {
        let hay = std::fs::read_dir(&d.paths[i])
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .any(|n| n.to_string_lossy().contains(".conflict-"));
        assert!(!hay, "d{i} no debe tener conflict files");
    }
    let manifiesto = std::fs::read_to_string(d.paths[0].join(".stalsync/manifest.jsonl")).unwrap();
    assert!(
        !manifiesto.contains(".conflict-"),
        "los conflict files no entran al manifiesto"
    );
}

#[test]
fn modificacion_concurrente_gana_sobre_borrado() {
    // PROMESA (política conservadora): si un disco borra y otro modifica,
    // gana la modificación. Borrar puede ser un accidente; una edición no
    // debe perderse. El archivo resucita en el disco que borró.
    let d = tres();
    d.write(0, "a.txt", "base");
    d.sync();

    d.remove(2, "a.txt");
    d.write(1, "a.txt", "editado");
    d.sync();

    for i in [0, 1, 2] {
        assert_eq!(d.read(i, "a.txt"), "editado", "d{i} debe tener la edición");
    }
}

#[test]
fn disco_nuevo_recibe_el_estado_acordado_completo() {
    // PROMESA: añadir un disco vacío al grupo lo trae al estado acordado
    // actual, aunque nadie haya "cambiado" nada desde el último sync.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "uno");
    d.write(0, "b.txt", "dos");
    d.sync();

    let d3 = TestDisks::new(1);
    let mut paths = d.paths.clone();
    paths.extend(d3.paths.clone());

    stalsync::sync::run(
        &paths,
        &stalsync::sync::Options {
            dry_run: false,
            verbose: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(d3.read(0, "a.txt"), "uno");
    assert_eq!(d3.read(0, "b.txt"), "dos");
    assert_eq!(d.tree_hash(0), d3.tree_hash(0));
}
