//! RESET EXPLODE para Linux: biblioteca de coleta de dados, testes de carga e ferramentas de PC.
//!
//! A interface de terminal (`tui`) e os subcomandos da CLI usam esta biblioteca; nada aqui depende da interface.

pub mod diag;
pub mod gpu;
pub mod pci;
pub mod sensors;
pub mod settings;
pub mod storage;
pub mod sysinfo;
pub mod util;
