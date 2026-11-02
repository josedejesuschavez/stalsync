//! Tests del binario CLI (main.rs): parseo de argumentos con clap, códigos
//! de salida scriptables y regresiones (un flag con valor no debe confundirse
//! con un disco). Cubre lo que las baterías de la lib no ven: main() entero.

mod common;

use assert_cmd::Command;
use common::TestDisks;

fn cli() -> Command {
    Command::cargo_bin("stalsync").unwrap()
}

fn args(disks: &TestDisks) -> Vec<String> {
    disks
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn version_y_help_salen_con_cero() {
    let out = cli().arg("--version").assert().code(0).get_output().clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("stalsync"));

    let out = cli().arg("--help").assert().code(0).get_output().clone();
    let help = String::from_utf8_lossy(&out.stdout);
    for sub in ["sync", "status", "refresh"] {
        assert!(help.contains(sub), "--help no menciona {sub}");
    }
}

#[test]
fn sin_subcomando_es_error_de_uso() {
    cli().assert().code(2);
}

#[test]
fn sync_con_un_solo_disco_es_error_de_uso() {
    let disks = TestDisks::new(1);
    cli().arg("sync").arg(&disks.paths[0]).assert().code(2);
}

#[test]
fn disco_inexistente_sale_con_1() {
    let disks = TestDisks::new(1);
    let out = cli()
        .arg("sync")
        .arg(&disks.paths[0])
        .arg("/no/existe/este/disco")
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stderr).contains("no existe"));
}

#[test]
fn trash_days_invalido_es_error_de_uso() {
    // REGRESIÓN: antes un valor no numérico se silenciaba y caía al default.
    let disks = TestDisks::new(2);
    let mut cmd = cli();
    cmd.arg("sync")
        .args(args(&disks))
        .arg("--trash-days")
        .arg("abc");
    cmd.assert().code(2);
}

#[test]
fn regresion_valor_de_trash_days_no_es_un_disco() {
    // REGRESIÓN (bug real): `sync a b --trash-days 7` trataba el "7" como
    // un tercer disco y fallaba. Debe sincronizar normal y respetar el flag.
    let disks = TestDisks::new(2);
    disks.write(0, "f.txt", "hola");

    let mut cmd = cli();
    cmd.arg("sync")
        .args(args(&disks))
        .arg("--trash-days")
        .arg("7");
    cmd.assert().code(0);

    assert_eq!(disks.read(1, "f.txt"), "hola");
}

#[test]
fn dry_run_planifica_sin_tocar_nada() {
    let disks = TestDisks::new(2);
    disks.write(0, "f.txt", "hola");

    let out = cli()
        .arg("sync")
        .args(args(&disks))
        .arg("--dry-run")
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("plan"));
    assert!(!disks.exists(1, "f.txt"), "dry-run no debe copiar");
}

#[test]
fn status_exit_code_scriptable() {
    let disks = TestDisks::new(2);
    disks.write(0, "f.txt", "hola");

    // Desfase pendiente → 1.
    cli().arg("status").args(args(&disks)).assert().code(1);

    // Tras converger → 0.
    cli().arg("sync").args(args(&disks)).assert().code(0);
    cli().arg("status").args(args(&disks)).assert().code(0);
}

#[test]
fn refresh_exige_confirmacion_sin_tty() {
    // Sin TTY y sin --yes debe negarse (exit 1); con --yes ejecuta (exit 0).
    let disks = TestDisks::new(2);
    let mut cmd = cli();
    cmd.arg("refresh")
        .args(args(&disks))
        .write_stdin("")
        .assert()
        .code(1);

    let mut cmd = cli();
    cmd.arg("refresh")
        .args(args(&disks))
        .arg("--yes")
        .assert()
        .code(0);
}

#[test]
fn sync_real_copia_y_reintento_es_idempotente() {
    let disks = TestDisks::new(2);
    disks.write(0, "dir/f.txt", "contenido");

    cli().arg("sync").args(args(&disks)).assert().code(0);
    assert_eq!(disks.read(1, "dir/f.txt"), "contenido");

    // Segunda pasada sin cambios: converge, nada que copiar.
    let out = cli()
        .arg("sync")
        .args(args(&disks))
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 copias (0 B)"));
}
