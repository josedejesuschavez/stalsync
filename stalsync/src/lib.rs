//! stalsync — sincronización N-way de discos locales.
//!
//! El DISEÑO no vive en este comentario ni en un MD: vive en los tests
//! (tests/merge_states.rs, tests/sync_e2e.rs, tests/resilience.rs).
//! Cada test protege una decisión de diseño y documenta su porqué.

pub mod manifest;
pub mod merge;
pub mod refresh;
pub mod scan;
pub mod status;
pub mod sync;
