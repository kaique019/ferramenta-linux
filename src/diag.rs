//! Registro de avisos e erros em memória. Substitui os `catch {}` vazios do projeto original:
//! falhas que não interrompem o programa ficam registradas e aparecem na tela "Registro" da TUI.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use crate::util::now_unix;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub time: u64,
    pub level: Level,
    pub source: String,
    pub message: String,
}

const CAPACITY: usize = 300;

fn store() -> &'static Mutex<VecDeque<Entry>> {
    static S: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Registra uma mensagem. As mais antigas são descartadas quando passa de 300.
pub fn log(level: Level, source: &str, message: impl Into<String>) {
    let message = message.into();
    if std::env::var_os("RESETEXPLODE_DEBUG").is_some() {
        eprintln!("[{level:?}] {source}: {message}");
    }
    if let Ok(mut q) = store().lock() {
        if q.len() >= CAPACITY {
            q.pop_front();
        }
        q.push_back(Entry { time: now_unix(), level, source: source.to_string(), message });
    }
}

pub fn info(source: &str, message: impl Into<String>) {
    log(Level::Info, source, message);
}

pub fn warn(source: &str, message: impl Into<String>) {
    log(Level::Warn, source, message);
}

pub fn error(source: &str, message: impl Into<String>) {
    log(Level::Error, source, message);
}

/// Cópia das mensagens registradas (mais antigas primeiro).
pub fn entries() -> Vec<Entry> {
    store().lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
}

/// Quantas mensagens de aviso ou erro existem.
pub fn problem_count() -> usize {
    store().lock().map(|q| q.iter().filter(|e| e.level != Level::Info).count()).unwrap_or(0)
}

pub fn clear() {
    if let Ok(mut q) = store().lock() {
        q.clear();
    }
}

/// Converte um `Result` em `Option`, registrando o erro em vez de descartá-lo em silêncio.
pub trait OrWarn<T> {
    fn or_warn(self, source: &str) -> Option<T>;
}

impl<T, E: std::fmt::Display> OrWarn<T> for Result<T, E> {
    fn or_warn(self, source: &str) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                warn(source, e.to_string());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_caps_entries() {
        // O armazenamento é global: usa mensagens com marca própria para não depender de outros testes.
        for i in 0..(CAPACITY + 20) {
            info("teste-diag", format!("marca-diag-{i}"));
        }
        let mine: Vec<_> = entries().into_iter().filter(|e| e.source == "teste-diag").collect();
        assert!(mine.len() <= CAPACITY);
        assert!(mine.last().unwrap().message.ends_with(&format!("{}", CAPACITY + 19)));
        assert!(entries().len() <= CAPACITY);
    }

    #[test]
    fn or_warn_logs_errors() {
        let r: Result<u32, String> = Err("falhou-xyz".into());
        assert_eq!(r.or_warn("teste-orwarn"), None);
        assert!(entries().iter().any(|e| e.source == "teste-orwarn" && e.message == "falhou-xyz" && e.level == Level::Warn));
        let ok: Result<u32, String> = Ok(3);
        assert_eq!(ok.or_warn("teste-orwarn"), Some(3));
    }
}
