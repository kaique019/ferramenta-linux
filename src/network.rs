//! Rede: interfaces, DNS, latência (conexão TCP, sem precisar de root), velocidade de download e bufferbloat.

use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::util::{Cancel, Roots, dir_names, natural_cmp, read_lines, read_num, read_trim};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Interface {
    pub name: String,
    /// "Ethernet", "Wi-Fi", "Loopback", "Virtual"...
    pub kind: String,
    pub up: bool,
    pub mac: String,
    pub mtu: Option<u32>,
    /// Velocidade do link em Mb/s (cabo).
    pub speed_mbps: Option<u32>,
    pub driver: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

fn kind_of(dir: &Path, name: &str) -> &'static str {
    if name == "lo" {
        "Loopback"
    } else if dir.join("wireless").exists() || dir.join("phy80211").exists() {
        "Wi-Fi"
    } else if dir.join("device").exists() {
        "Ethernet"
    } else {
        "Virtual"
    }
}

pub fn interfaces(r: &Roots) -> Vec<Interface> {
    let base = r.sys.join("class/net");
    let mut names = dir_names(&base);
    names.sort_by(|a, b| natural_cmp(a, b));
    names
        .into_iter()
        .map(|n| {
            let d = base.join(&n);
            let driver = std::fs::read_link(d.join("device/driver")).ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
            Interface {
                kind: kind_of(&d, &n).into(),
                up: read_trim(d.join("operstate")).is_some_and(|s| s == "up" || s == "unknown") && n != "lo",
                mac: read_trim(d.join("address")).unwrap_or_default(),
                mtu: read_num(d.join("mtu")),
                // O kernel informa -1 quando o link está caído ou não tem velocidade definida.
                speed_mbps: read_num::<i64>(d.join("speed")).filter(|v| *v > 0).map(|v| v as u32),
                driver,
                rx_bytes: read_num(d.join("statistics/rx_bytes")).unwrap_or(0),
                tx_bytes: read_num(d.join("statistics/tx_bytes")).unwrap_or(0),
                name: n,
            }
        })
        .collect()
}

/// Servidores DNS de /etc/resolv.conf (e de /run/systemd/resolve/resolv.conf quando o primeiro só aponta para o stub local).
pub fn dns_servers(r: &Roots) -> Vec<String> {
    fn parse(path: &Path) -> Vec<String> {
        read_lines(path).iter().filter_map(|l| l.trim().strip_prefix("nameserver").map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
    }
    let mut v = parse(&r.etc.join("resolv.conf"));
    if v.iter().all(|s| s.starts_with("127.")) {
        let real = parse(Path::new("/run/systemd/resolve/resolv.conf"));
        if !real.is_empty() && r.etc == Path::new("/etc") {
            v = real;
        }
    }
    v
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LatencyStats {
    pub target: String,
    pub sent: u32,
    pub received: u32,
    pub loss_pct: f64,
    pub min_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub median_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub max_ms: Option<f64>,
    /// Variação média entre medidas consecutivas.
    pub jitter_ms: Option<f64>,
}

pub fn stats(target: &str, sent: u32, samples: &[f64]) -> LatencyStats {
    let mut s = LatencyStats { target: target.into(), sent, received: samples.len() as u32, loss_pct: if sent == 0 { 0.0 } else { (sent as f64 - samples.len() as f64) / sent as f64 * 100.0 }, ..Default::default() };
    if samples.is_empty() {
        return s;
    }
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    s.min_ms = Some(v[0]);
    s.max_ms = v.last().copied();
    s.avg_ms = Some(v.iter().sum::<f64>() / v.len() as f64);
    s.median_ms = Some(at(0.5));
    s.p95_ms = Some(at(0.95));
    if samples.len() > 1 {
        s.jitter_ms = Some(samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (samples.len() - 1) as f64);
    }
    s
}

/// Mede o tempo de abrir uma conexão TCP (equivalente a um ping que não precisa de privilégios).
pub fn tcp_latency(host: &str, port: u16, count: u32, timeout: Duration, cancel: &Cancel) -> LatencyStats {
    let target = format!("{host}:{port}");
    let addr = target.to_socket_addrs().ok().and_then(|mut a| a.next());
    let mut samples = vec![];
    if let Some(addr) = addr {
        for i in 0..count {
            if cancel.is_cancelled() {
                break;
            }
            let t = Instant::now();
            if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            if i + 1 < count {
                cancel.sleep(Duration::from_millis(100));
            }
        }
    }
    stats(&target, count, &samples)
}

/// Tempo de resolução de um nome pelo resolvedor do sistema, em ms (o cache pode afetar repetições).
pub fn dns_time(name: &str) -> Option<f64> {
    let t = Instant::now();
    (name, 80).to_socket_addrs().ok()?.next()?;
    Some(t.elapsed().as_secs_f64() * 1000.0)
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadResult {
    pub url: String,
    pub bytes: u64,
    pub seconds: f64,
    pub mbps: f64,
}

/// Baixa `url` por até `max_secs` (ou `max_bytes`) e calcula a velocidade em Mb/s.
pub fn download(url: &str, max_secs: f64, max_bytes: u64, cancel: &Cancel, stop: &AtomicBool) -> Result<DownloadResult, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs_f64(max_secs + 10.0))).build().into();
    let resp = agent.get(url).call().map_err(|e| format!("não conseguiu baixar {url}: {e}"))?;
    let mut body = resp.into_body();
    let mut rd = body.as_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let (mut total, start) = (0u64, Instant::now());
    loop {
        if cancel.is_cancelled() || stop.load(Ordering::Relaxed) || start.elapsed().as_secs_f64() >= max_secs || total >= max_bytes {
            break;
        }
        match rd.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(e) => {
                if total == 0 {
                    return Err(format!("falha na leitura: {e}"));
                }
                break;
            }
        }
    }
    let seconds = start.elapsed().as_secs_f64().max(1e-6);
    Ok(DownloadResult { url: url.into(), bytes: total, seconds, mbps: total as f64 * 8.0 / seconds / 1e6 })
}

#[derive(Debug, Clone, Serialize)]
pub struct Bufferbloat {
    pub idle: LatencyStats,
    pub loaded: LatencyStats,
    pub download: Option<DownloadResult>,
    /// Aumento da latência mediana sob carga, em ms.
    pub added_ms: Option<f64>,
    /// "A" (excelente) a "F".
    pub grade: &'static str,
}

pub fn grade(added_ms: f64) -> &'static str {
    match added_ms {
        x if x < 5.0 => "A",
        x if x < 30.0 => "B",
        x if x < 60.0 => "C",
        x if x < 200.0 => "D",
        _ => "F",
    }
}

/// Compara a latência com a linha parada e durante um download (fila cheia no roteador = jogo com lag).
pub fn bufferbloat(host: &str, port: u16, url: &str, secs: f64, cancel: &Cancel) -> Bufferbloat {
    let timeout = Duration::from_secs(2);
    let idle = tcp_latency(host, port, 8, timeout, cancel);
    let stop = Arc::new(AtomicBool::new(false));
    let (u, c, s) = (url.to_string(), cancel.clone(), stop.clone());
    let worker = std::thread::spawn(move || download(&u, secs, 2 << 30, &c, &s));
    cancel.sleep(Duration::from_millis(1500)); // deixa o download encher a fila
    let loaded = tcp_latency(host, port, ((secs - 1.5).max(1.0) * 4.0) as u32, timeout, cancel);
    stop.store(true, Ordering::Relaxed);
    let download = worker.join().ok().and_then(|r| r.ok());
    let added_ms = idle.median_ms.zip(loaded.median_ms).map(|(a, b)| (b - a).max(0.0));
    Bufferbloat { grade: added_ms.map(grade).unwrap_or("?"), idle, loaded, download, added_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::net::TcpListener;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn lists_interfaces_by_kind() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "sys/class/net/lo/operstate", "unknown\n");
        write(d.path(), "sys/class/net/enp3s0/operstate", "up\n");
        write(d.path(), "sys/class/net/enp3s0/device/x", "");
        write(d.path(), "sys/class/net/enp3s0/speed", "2500\n");
        write(d.path(), "sys/class/net/enp3s0/address", "aa:bb:cc:dd:ee:ff\n");
        write(d.path(), "sys/class/net/enp3s0/statistics/rx_bytes", "1000\n");
        write(d.path(), "sys/class/net/wlp2s0/operstate", "down\n");
        write(d.path(), "sys/class/net/wlp2s0/wireless/x", "");
        write(d.path(), "sys/class/net/wlp2s0/speed", "-1\n");
        write(d.path(), "sys/class/net/docker0/operstate", "up\n");
        let v = interfaces(&Roots::under(d.path()));
        let by = |n: &str| v.iter().find(|i| i.name == n).unwrap();
        assert_eq!(by("lo").kind, "Loopback");
        assert!(!by("lo").up);
        assert_eq!((by("enp3s0").kind.as_str(), by("enp3s0").up, by("enp3s0").speed_mbps, by("enp3s0").rx_bytes), ("Ethernet", true, Some(2500), 1000));
        assert_eq!((by("wlp2s0").kind.as_str(), by("wlp2s0").up, by("wlp2s0").speed_mbps), ("Wi-Fi", false, None));
        assert_eq!(by("docker0").kind, "Virtual");
    }

    #[test]
    fn reads_dns_servers() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "etc/resolv.conf", "# x\nsearch lan\nnameserver 1.1.1.1\nnameserver   8.8.8.8\noptions edns0\n");
        assert_eq!(dns_servers(&Roots::under(d.path())), ["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn computes_latency_stats() {
        let s = stats("x", 5, &[10.0, 20.0, 12.0, 11.0]);
        assert_eq!((s.received, s.loss_pct), (4, 20.0));
        assert_eq!((s.min_ms, s.max_ms), (Some(10.0), Some(20.0)));
        assert_eq!(s.avg_ms, Some(13.25));
        assert_eq!(s.jitter_ms, Some((10.0 + 8.0 + 1.0) / 3.0));
        let none = stats("x", 3, &[]);
        assert_eq!((none.loss_pct, none.avg_ms), (100.0, None));
        assert_eq!(stats("x", 0, &[]).loss_pct, 0.0);
    }

    #[test]
    fn grades_bufferbloat() {
        assert_eq!([grade(0.0), grade(10.0), grade(40.0), grade(100.0), grade(500.0)], ["A", "B", "C", "D", "F"]);
    }

    #[test]
    fn tcp_latency_against_local_listener_and_closed_port() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let s = tcp_latency("127.0.0.1", port, 3, Duration::from_secs(1), &Cancel::new());
        assert_eq!(s.received, 3);
        assert!(s.avg_ms.unwrap() < 500.0);
        drop(l);
        let closed = tcp_latency("127.0.0.1", port, 2, Duration::from_millis(300), &Cancel::new());
        assert_eq!(closed.received, 0);
        assert_eq!(closed.loss_pct, 100.0);
        assert!(tcp_latency("host.invalido.test", 80, 1, Duration::from_millis(300), &Cancel::new()).received == 0);
    }

    fn serve(body_len: usize) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                std::thread::spawn(move || {
                    let mut req = [0u8; 1024];
                    let _ = s.read(&mut req);
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n");
                    let chunk = vec![7u8; 64 * 1024];
                    let mut left = body_len;
                    while left > 0 {
                        let n = left.min(chunk.len());
                        if s.write_all(&chunk[..n]).is_err() {
                            break;
                        }
                        left -= n;
                    }
                });
            }
        });
        port
    }

    #[test]
    fn download_measures_bytes_and_respects_limits() {
        let port = serve(4 << 20);
        let url = format!("http://127.0.0.1:{port}/x");
        let r = download(&url, 10.0, u64::MAX, &Cancel::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.bytes, 4 << 20);
        assert!(r.mbps > 1.0);
        let capped = download(&url, 10.0, 128 * 1024, &Cancel::new(), &AtomicBool::new(false)).unwrap();
        assert!(capped.bytes >= 128 * 1024 && capped.bytes < 4 << 20);
        let c = Cancel::new();
        c.cancel();
        assert_eq!(download(&url, 10.0, u64::MAX, &c, &AtomicBool::new(false)).unwrap().bytes, 0);
        assert!(download("http://127.0.0.1:1/x", 1.0, 10, &Cancel::new(), &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn bufferbloat_runs_end_to_end_locally() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        std::thread::spawn(move || for _ in l.incoming() {});
        let dl = serve(512 << 20);
        let b = bufferbloat("127.0.0.1", p, &format!("http://127.0.0.1:{dl}/x"), 3.0, &Cancel::new());
        assert!(b.idle.received > 0 && b.loaded.received > 0);
        assert!(b.download.is_some_and(|d| d.bytes > 0));
        assert_ne!(b.grade, "?");
    }
}
