//! Processos que mais usam CPU e memória, lidos de /proc/<pid>/stat.
//!
//! O uso de CPU vem da diferença entre duas leituras (como o `top`): 100% = um núcleo inteiro,
//! então um processo com várias threads passa de 100%.

use std::collections::HashMap;
use std::fs;

use crate::util::Roots;

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// Uso de CPU desde a leitura anterior (100 = um núcleo).
    pub cpu: f64,
    /// Memória residente em bytes.
    pub rss: u64,
}

/// Interpreta uma linha de /proc/<pid>/stat: (nome, ticks de CPU usados, páginas residentes).
/// O nome fica entre parênteses e pode ter espaços e até parênteses, por isso a busca pelo último `)`.
pub fn parse_stat(line: &str) -> Option<(String, u64, u64)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let name = line[open + 1..close].to_string();
    // depois do nome o primeiro campo é o 3º (estado); utime=14, stime=15, rss=24 na numeração do man
    let f: Vec<&str> = line[close + 1..].split_whitespace().collect();
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let rss: i64 = f.get(21)?.parse().ok()?;
    Some((name, utime + stime, rss.max(0) as u64))
}

pub struct ProcSampler {
    proc: std::path::PathBuf,
    prev: HashMap<u32, u64>,
    prev_t: Option<f64>,
    page: u64,
    hz: f64,
}

impl ProcSampler {
    pub fn new(r: &Roots) -> Self {
        // SAFETY: sysconf só consulta constantes do sistema.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        ProcSampler { proc: r.proc.clone(), prev: HashMap::new(), prev_t: None, page, hz }
    }

    /// Lê todos os processos; `now` é o relógio em segundos (só a diferença importa).
    /// Na primeira chamada não há com o que comparar e o uso de CPU sai 0.
    pub fn sample_at(&mut self, now: f64) -> Vec<Proc> {
        let dt = self.prev_t.map(|t| now - t).filter(|d| *d > 0.05);
        let mut cur = HashMap::new();
        let mut out = vec![];
        if let Ok(rd) = fs::read_dir(&self.proc) {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
                let Ok(text) = fs::read_to_string(e.path().join("stat")) else { continue };
                let Some((name, ticks, pages)) = parse_stat(&text) else { continue };
                let cpu = match (dt, self.prev.get(&pid)) {
                    (Some(dt), Some(p)) => ticks.saturating_sub(*p) as f64 / self.hz / dt * 100.0,
                    _ => 0.0,
                };
                cur.insert(pid, ticks);
                out.push(Proc { pid, name, cpu, rss: pages * self.page });
            }
        }
        self.prev = cur;
        self.prev_t = Some(now);
        out
    }

    pub fn sample(&mut self) -> Vec<Proc> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        self.sample_at(now)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortBy {
    #[default]
    Cpu,
    Mem,
}

/// Os `n` primeiros por CPU ou memória (empate pelo pid, para a ordem não ficar pulando).
pub fn top(mut v: Vec<Proc>, by: SortBy, n: usize) -> Vec<Proc> {
    match by {
        SortBy::Cpu => v.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)).then(a.pid.cmp(&b.pid))),
        SortBy::Mem => v.sort_by(|a, b| b.rss.cmp(&a.rss).then(a.pid.cmp(&b.pid))),
    }
    v.truncate(n);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(pid: u32, name: &str, ut: u64, st: u64, rss: i64) -> String {
        // 52 campos como no kernel; só importam o nome, utime, stime e rss
        let mut f = vec!["0".to_string(); 50];
        f[0] = "S".into();
        f[11] = ut.to_string();
        f[12] = st.to_string();
        f[21] = rss.to_string();
        format!("{pid} ({name}) {}", f.join(" "))
    }

    fn put(root: &std::path::Path, pid: u32, text: &str) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("stat"), text).unwrap();
    }

    #[test]
    fn parses_names_with_spaces_and_parentheses() {
        assert_eq!(parse_stat(&stat(1, "systemd", 10, 5, 300)), Some(("systemd".into(), 15, 300)));
        assert_eq!(parse_stat(&stat(2, "Web Content (x) y", 1, 2, 3)).unwrap().0, "Web Content (x) y");
        assert_eq!(parse_stat("lixo"), None);
        assert_eq!(parse_stat("1 (a) S 2"), None, "linha curta");
        assert_eq!(parse_stat(&stat(3, "neg", 0, 0, -5)).unwrap().2, 0, "rss negativo vira 0");
    }

    #[test]
    fn cpu_is_a_delta_between_two_samples() {
        let d = tempfile::tempdir().unwrap();
        let r = Roots::under(d.path());
        fs::create_dir_all(&r.proc).unwrap();
        put(&r.proc, 10, &stat(10, "busy", 100, 0, 1000));
        put(&r.proc, 11, &stat(11, "idle", 5, 0, 10));
        fs::create_dir_all(r.proc.join("self")).unwrap(); // pastas que não são pid são ignoradas
        let mut s = ProcSampler::new(&r);
        let first = s.sample_at(100.0);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|p| p.cpu == 0.0), "primeira leitura não tem referência");
        let hz = s.hz as u64;
        // em 2 s o "busy" usou 1,5 núcleo
        put(&r.proc, 10, &stat(10, "busy", 100 + hz * 3, 0, 1000));
        put(&r.proc, 11, &stat(11, "idle", 5, 0, 10));
        let v = top(s.sample_at(102.0), SortBy::Cpu, 5);
        assert_eq!(v[0].name, "busy");
        assert!((v[0].cpu - 150.0).abs() < 0.5, "{}", v[0].cpu);
        assert_eq!(v[1].cpu, 0.0);
        assert_eq!(v[0].rss, 1000 * s.page);
    }

    #[test]
    fn new_and_vanished_processes_do_not_break_the_delta() {
        let d = tempfile::tempdir().unwrap();
        let r = Roots::under(d.path());
        fs::create_dir_all(&r.proc).unwrap();
        put(&r.proc, 1, &stat(1, "a", 50, 0, 1));
        let mut s = ProcSampler::new(&r);
        s.sample_at(0.0);
        fs::remove_dir_all(r.proc.join("1")).unwrap();
        put(&r.proc, 2, &stat(2, "novo", 9999, 0, 1));
        let v = s.sample_at(1.0);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cpu, 0.0, "processo novo não herda um contador enorme");
        // contador que diminui (pid reaproveitado) não dá número negativo nem estoura
        put(&r.proc, 2, &stat(2, "novo", 1, 0, 1));
        assert_eq!(s.sample_at(2.0)[0].cpu, 0.0);
    }

    #[test]
    fn top_sorts_and_truncates() {
        let p = |pid, cpu, rss| Proc { pid, name: format!("p{pid}"), cpu, rss };
        let v = vec![p(1, 5.0, 100), p(2, 50.0, 10), p(3, 50.0, 900), p(4, 0.0, 5000)];
        assert_eq!(top(v.clone(), SortBy::Cpu, 3).iter().map(|x| x.pid).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(top(v, SortBy::Mem, 2).iter().map(|x| x.pid).collect::<Vec<_>>(), vec![4, 3]);
    }
}
