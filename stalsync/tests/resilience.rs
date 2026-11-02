//! Batería 3: resiliencia e invariantes de diseño.
//!
//! Estos tests son la razón de ser del modelo "manifiesto = caché, no verdad":
//! demuestran que perder o desincronizar estado interno nunca corrompe datos.

mod common;

use common::{tree_hash, TestDisks};

#[test]
fn manifiesto_borrado_se_reconstruye_sin_copiar() {
    // INVARIANTE: el manifiesto es caché. Borrarlo entero solo cuesta rehash;
    // el contenido idéntico no se recopia (la verdad es el hash en disco).
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "contenido");
    d.write(0, "sub/b.txt", "contenido b");
    d.sync();

    std::fs::remove_dir_all(d.paths[1].join(".stalsync")).unwrap();

    // El disco sin manifiesto reporta todo como Changed, pero sus hashes ya
    // son los acordados → el merge no debe generar copias hacia él.
    let (copias, ..) = d.sync_stats();
    assert_eq!(copias, 0, "contenido idéntico no se recopia aunque el manifiesto se perdiera");
    d.sync();

    assert_eq!(d.tree_hash(0), d.tree_hash(1));
    assert_eq!(d.tree_hash(0), d.tree_hash(2));
}

#[test]
fn manifiesto_corrupto_se_tolerante() {
    // INVARIANTE: una línea malformada del manifiesto no tumba el sync;
    // se ignora con advertencia y esa ruta se trata como desconocida.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "ok");
    d.write(0, "b.txt", "ok b");
    d.sync();

    let mp = d.paths[1].join(".stalsync/manifest.jsonl");
    let mut contenido = std::fs::read_to_string(&mp).unwrap();
    contenido.push_str("{esto no es json}\n");
    std::fs::write(&mp, contenido).unwrap();

    d.sync(); // no debe fallar
    assert_eq!(d.read(1, "a.txt"), "ok");
    assert_eq!(d.tree_hash(0), d.tree_hash(1));
}

#[test]
fn escritura_de_manifiesto_es_atomica_no_deja_tmp() {
    // INVARIANTE: save hace tmp + fsync + rename. Nunca queda un .tmp
    // visible (que el siguiente load confundiría con manifiesto válido).
    let d = TestDisks::new(1);
    d.write(0, "a.txt", "x");
    d.sync();
    let dir = d.paths[0].join(".stalsync");
    let restos: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(restos.is_empty(), "no deben quedar temporales de manifiesto");
}

#[test]
fn disco_desconectado_converge_al_reincorporarse() {
    // INVARIANTE: cada disco se mide contra SU propio manifiesto, no contra
    // un estado global. Si el grupo avanza sin un disco, al reincorporarse
    // su delta se detecta correctamente y converge sin pérdidas.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "v1");
    d.write(0, "b.txt", "fijo");
    d.sync();

    // d2 "desconectado": sync solo de d1 y d3
    let dos = vec![d.paths[0].clone(), d.paths[2].clone()];
    d.write(0, "a.txt", "v2");
    stalsync::sync::run(&dos, &stalsync::sync::Options { dry_run: false, verbose: false, ..Default::default() }).unwrap();

    // d2 vuelve con su manifiesto viejo: debe recibir v2 y seguir teniendo b.txt
    d.sync();
    assert_eq!(d.read(1, "a.txt"), "v2");
    assert_eq!(d.read(1, "b.txt"), "fijo");
    assert_eq!(d.tree_hash(0), d.tree_hash(1));
    assert_eq!(d.tree_hash(0), d.tree_hash(2));
}

#[test]
fn convergencia_desde_estado_arbitrario() {
    // INVARIANTE (la propiedad central): desde cualquier combinación de
    // altas, ediciones y borrados repartidos entre discos, una pasada
    // deja a todos con el mismo contenido, y la siguiente es un no-op.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "a");
    d.write(0, "b.txt", "b");
    d.write(0, "c.txt", "c");
    d.write(0, "d.txt", "d");
    d.sync();

    // Caos controlado: cada disco toca archivos distintos.
    d.write(0, "a.txt", "a editado en d1"); // d1 edita a
    d.write(1, "nuevo_en_d2.txt", "nuevo"); // d2 crea
    d.remove(2, "b.txt"); // d3 borra b
    d.write(2, "c.txt", "c editado en d3"); // d3 edita c

    d.sync();

    let h0 = d.tree_hash(0);
    assert_eq!(h0, d.tree_hash(1), "d1 y d2 convergen");
    assert_eq!(h0, d.tree_hash(2), "d1 y d3 convergen");

    // Punto fijo: la siguiente pasada no hace nada.
    assert_eq!(d.sync_stats(), (0, 0, 0));

    // Y el contenido es el esperado ruta por ruta.
    assert_eq!(d.read(0, "a.txt"), "a editado en d1");
    assert_eq!(d.read(0, "nuevo_en_d2.txt"), "nuevo");
    assert!(!d.exists(0, "b.txt"));
    assert_eq!(d.read(0, "c.txt"), "c editado en d3");
    assert_eq!(d.read(0, "d.txt"), "d");
}

#[test]
fn doble_sync_es_estable_incluso_con_conflicto_resuelto() {
    // INVARIANTE: tras un conflicto, el estado queda estable: el conflict
    // file (fuera del manifiesto) no vuelve a entrar al sync y no se
    // propaga en pasadas siguientes.
    let d = TestDisks::new(3);
    d.write(0, "a.txt", "base");
    d.sync();

    d.write(0, "a.txt", "v d1");
    d.set_mtime(0, "a.txt", 1_700_000_000, 0);
    d.write(1, "a.txt", "v d2");
    d.set_mtime(1, "a.txt", 1_700_050_000, 0);

    d.sync();
    // Segunda pasada: el conflict file de d0 existe en disco pero excluido.
    assert_eq!(d.sync_stats(), (0, 0, 0));

    // Tercera pasada tras editar otra cosa: el conflict file sigue quieto.
    d.write(2, "otro.txt", "nuevo");
    d.sync();
    let conflictos_en_d0: Vec<_> = std::fs::read_dir(&d.paths[0])
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains(".conflict-"))
        .collect();
    assert_eq!(conflictos_en_d0.len(), 1, "el conflict file original sigue intacto y único");
    for i in [1, 2] {
        let hay = std::fs::read_dir(&d.paths[i])
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().contains(".conflict-"));
        assert!(!hay, "nada propagó el conflict file a d{i}");
    }
}

#[test]
fn tree_hash_cuenta_rutas_y_contenido() {
    // Sanidad del helper usado por los demás tests: mismo contenido en
    // distinta ruta => distinto hash; distinto contenido misma ruta => distinto.
    let a = TestDisks::new(1);
    let b = TestDisks::new(1);
    a.write(0, "x.txt", "mismo");
    b.write(0, "y.txt", "mismo");
    assert_ne!(tree_hash(&a.paths[0]), tree_hash(&b.paths[0]));

    b.write(0, "y.txt", "distinto");
    let antes = tree_hash(&a.paths[0]);
    a.write(0, "x.txt", "cambiado");
    assert_ne!(antes, tree_hash(&a.paths[0]));
}
