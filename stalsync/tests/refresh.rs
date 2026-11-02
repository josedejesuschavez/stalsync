//! Batería 5: el comando `refresh` (re-baseline de manifiestos).
//!
//! INVARIANTE: refresh reescribe el manifiesto de cada disco para que refleje
//! su contenido actual, SIN copiar ni borrar archivos. Es la herramienta de
//! recuperación/reconstrucción de la caché. Su contrapartida documentada:
//! los cambios pendientes de propagar dejan de estarlo.

mod common;

use common::TestDisks;
use stalsync::manifest;
use stalsync::refresh::refresh;
use stalsync::status::compute_status;

#[test]
fn reconstruye_manifiesto_perdido() {
    // CASO: el manifiesto de d2 se perdió (es caché, puede pasar). refresh
    // lo reconstruye desde el contenido real; d2 vuelve a estar al día.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "x");
    d.write(0, "b.txt", "y");
    d.sync();

    std::fs::remove_dir_all(d.paths[1].join(".stalsync")).unwrap();
    refresh(&d.paths).unwrap();

    // El manifiesto existe otra vez y refleja el contenido.
    let m = manifest::load(&d.paths[1]).unwrap();
    assert_eq!(m.len(), 2);
    assert!(m.contains_key(std::path::Path::new("a.txt")));

    // Y el estado vuelve a ser convergido (sin copias pendientes).
    assert!(compute_status(&d.paths).unwrap().convergido);
}

#[test]
fn actualiza_hashes_tras_cp_sin_preservar_mtime() {
    // CASO: el usuario copió archivos a mano (cp) y los mtimes cambiaron.
    // Un sync haría esto mismo pagando copias innecesarias; refresh solo
    // actualiza el manifiesto (rehashea y guarda el mtime nuevo).
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "contenido");
    d.sync();

    // Simula cp: mismo contenido, mtime nuevo.
    d.set_mtime(1, "a.txt", 1_750_000_000, 0);

    let reports = refresh(&d.paths).unwrap();
    assert_eq!(reports[1].actualizadas, 1);
    assert_eq!(reports[1].intactas, 0);

    // El manifiesto quedó con el mtime nuevo; un sync posterior no copia nada.
    let m = manifest::load(&d.paths[1]).unwrap();
    let e = &m[std::path::Path::new("a.txt")];
    assert_eq!(e.mtime_ns, 1_750_000_000_000_000_000);
    assert!(compute_status(&d.paths).unwrap().convergido);
}

#[test]
fn olvida_borrado_pendiente() {
    // SEMÁNTICA DOCUMENTADA: un borrado aún no propagado se olvida tras
    // refresh. El disco que borró queda "como si nunca lo hubiera tenido",
    // así que el siguiente sync vuelve a traer el archivo desde los demás.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "x");
    d.sync();

    d.remove(0, "a.txt"); // borrado pendiente (no sincronizado)
    refresh(&d.paths).unwrap();

    // El manifiesto de d0 ya no contiene la entrada.
    let m = manifest::load(&d.paths[0]).unwrap();
    assert!(!m.contains_key(std::path::Path::new("a.txt")));

    // El borrado ya no se propaga: d0 recibe el archivo de vuelta.
    let r = compute_status(&d.paths).unwrap();
    assert_eq!(r.disks[0].recibe, 1);
}

#[test]
fn no_copia_ni_borra_archivos() {
    // INVARIANTE DURO: refresh solo toca .stalsync/. Los archivos de datos
    // quedan byte por byte iguales, incluidos los parciales/conflictos.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "contenido A");
    d.sync();
    d.write(1, "a.txt", "contenido distinto B"); // desfase activo

    let h_antes = (d.tree_hash(0), d.tree_hash(1), d.tree_hash(2));
    refresh(&d.paths).unwrap();
    let h_despues = (d.tree_hash(0), d.tree_hash(1), d.tree_hash(2));

    assert_eq!(h_antes, h_despues, "refresh no debe tocar datos");
}

#[test]
fn integra_cambios_hechos_a_mano() {
    // CASO: el usuario resolvió conflictos a mano (borró los .conflict-*
    // y dejó la versión que quería). refresh materializa esa realidad.
    let d = TestDisks::new(2);
    d.write(0, "a.txt", "base");
    d.sync();

    d.write(0, "a.txt", "v d0");
    d.set_mtime(0, "a.txt", 1_700_000_000, 0);
    d.write(1, "a.txt", "v d1");
    d.set_mtime(1, "a.txt", 1_700_100_000, 0);
    d.sync(); // d1 gana; d0 conserva su versión como .conflict-

    // El usuario decide quedarse con la versión de d0: la restaura a mano
    // y borra el conflict file.
    let conflict = std::fs::read_dir(&d.paths[0])
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".conflict-")
        })
        .unwrap();
    std::fs::rename(&conflict, d.paths[0].join("a.txt")).unwrap();

    refresh(&d.paths).unwrap();

    // La versión elegida a mano queda reflejada en el manifiesto.
    let m = manifest::load(&d.paths[0]).unwrap();
    assert_eq!(d.read(0, "a.txt"), "v d0");
    let _ = m; // el manifiesto coincide con el contenido (verificado por scan)
    assert_eq!(compute_status(&d.paths).unwrap().disks[0].cambios, 0);
}
