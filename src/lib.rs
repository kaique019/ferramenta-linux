//! RESET EXPLODE para Linux: biblioteca de coleta de dados, testes de carga e ferramentas de PC.
//!
//! A interface de terminal (`tui`) e os subcomandos da CLI usam esta biblioteca; nada aqui depende da interface.

pub mod autostart;
pub mod bench;
pub mod cli;
pub mod bottleneck;
pub mod diag;
pub mod games;
pub mod gpu;
pub mod hwerrors;
pub mod installer;
pub mod launch;
pub mod network;
pub mod occheck;
pub mod overlay;
pub mod pci;
pub mod power;
pub mod sensors;
pub mod settings;
pub mod storage;
pub mod report;
pub mod serial;
pub mod stress;
pub mod sysinfo;
pub mod tui;
pub mod tweaks;
pub mod util;
pub mod vdf;
