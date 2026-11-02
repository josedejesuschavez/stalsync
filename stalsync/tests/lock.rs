//! Tests del lock entre procesos: dos instancias de stalsync no deben
//! operar sobre los mismos discos a la vez, sin importar el orden en que
//! se pasen los discos en la línea de comandos.

use stalsync::lock;

fn disk_set(tmp: &std::path::Path) -> Vec<std::path::PathBuf> {
    let a = tmp.join("a");
    let b = tmp.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    vec![a, b]
}

#[test]
fn segundo_adquirente_falla_mientras_hay_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let disks = disk_set(tmp.path());
    let _l1 = lock::acquire_all(&disks).unwrap();

    let err = lock::acquire_all(&disks).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    assert!(err.to_string().contains("otro proceso stalsync"));
}

#[test]
fn orden_invertido_de_discos_no_deadlockea_ni_entra() {
    let tmp = tempfile::tempdir().unwrap();
    let mut disks = disk_set(tmp.path());
    let _l1 = lock::acquire_all(&disks).unwrap();

    // Mismos discos en orden inverso: debe fallar rápido (try_lock), no
    // quedarse esperando ni interbloquearse.
    disks.reverse();
    let err = lock::acquire_all(&disks).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn al_liberar_se_puede_readquirir() {
    let tmp = tempfile::tempdir().unwrap();
    let disks = disk_set(tmp.path());
    {
        let _l1 = lock::acquire_all(&disks).unwrap();
    } // drop libera
    let _l2 = lock::acquire_all(&disks).unwrap();
}

#[test]
fn discos_sin_stalsync_previo_tambien_bloquean() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("solo");
    std::fs::create_dir_all(&a).unwrap();
    let _l1 = lock::acquire_all(std::slice::from_ref(&a)).unwrap();
    // acquire_all creó .stalsync/lock aunque no existiera antes.
    assert!(a.join(".stalsync").join("lock").is_file());
    assert!(lock::acquire_all(&[a]).is_err());
}
