//! stalsync — sincronización N-way de discos locales.
//!
//! El DISEÑO no vive en este comentario ni en un MD: vive en los tests
//! (tests/merge_states.rs, tests/sync_e2e.rs, tests/resilience.rs).
//! Cada test protege una decisión de diseño y documenta su porqué.

pub mod lock;
pub mod manifest;
pub mod merge;
pub mod refresh;
pub mod scan;
pub mod status;
pub mod sync;
pub mod trash;

/// Enriquece un `io::Error` con contexto: qué operación, sobre qué disco o
/// archivo. El mensaje original se conserva al final: "contexto: causa".
/// El closure solo se evalúa si hubo error.
pub(crate) trait Ctx<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> std::io::Result<T>;
}

impl<T> Ctx<T> for std::io::Result<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> std::io::Result<T> {
        self.map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", context())))
    }
}
