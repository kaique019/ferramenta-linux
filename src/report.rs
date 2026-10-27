//! Relatório do sistema em JSON ou HTML (um arquivo só, sem scripts nem recursos externos).

use serde::Serialize;

use crate::settings::Settings;
use crate::util::{Roots, fmt_bytes, format_local, html_escape, now_unix};
use crate::{gpu, power, sensors, storage, sysinfo};

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub generated_at: u64,
    pub os: sysinfo::OsInfo,
    pub board: sysinfo::BoardInfo,
    pub cpu: sysinfo::CpuInfo,
    pub memory: sysinfo::MemInfo,
    pub gpus: Vec<gpu::GpuInfo>,
    pub disks: Vec<storage::Disk>,
    pub mounts: Vec<storage::Mount>,
    pub sensors: Vec<sensors::SensorReading>,
    pub power: power::PowerInfo,
    pub bench_records: Vec<crate::settings::BenchRecord>,
    pub oc_history: Vec<crate::settings::OcRecord>,
}

pub fn collect(r: &Roots, settings: &Settings, smart: bool) -> Report {
    Report {
        generated_at: now_unix(),
        os: sysinfo::collect_os(r),
        board: sysinfo::collect_board(r),
        cpu: sysinfo::collect_cpu(r),
        memory: sysinfo::collect_mem(r, true),
        gpus: gpu::collect(r),
        disks: storage::collect_disks(r, smart),
        mounts: storage::collect_mounts(r),
        sensors: sensors::read_all(r),
        power: power::collect(r),
        bench_records: settings.bench_records.clone(),
        oc_history: settings.oc_history.clone(),
    }
}

fn row(k: &str, v: &str) -> String {
    format!("<tr><th>{}</th><td>{}</td></tr>\n", html_escape(k), html_escape(v))
}

fn opt<T: std::fmt::Display>(v: &Option<T>, unit: &str) -> String {
    v.as_ref().map(|x| format!("{x}{unit}")).unwrap_or_else(|| "—".into())
}

pub fn to_html(rep: &Report) -> String {
    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"pt-BR\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Relatório Pingu</title>\n<style>\n:root{color-scheme:light dark;--bg:#fff;--fg:#1c1c1e;--mut:#6b6b70;--line:#d8d8dd;--card:#f5f5f7}\n@media(prefers-color-scheme:dark){:root{--bg:#161618;--fg:#ececf0;--mut:#9a9aa2;--line:#323238;--card:#202024}}\nbody{margin:0;padding:24px 16px;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}main{max-width:860px;margin:auto}\nh1{font-size:1.6rem;margin:0 0 4px}h2{font-size:1.1rem;margin:28px 0 8px;border-bottom:1px solid var(--line);padding-bottom:4px}\n.mut{color:var(--mut)}table{border-collapse:collapse;width:100%;background:var(--card);border-radius:8px;overflow:hidden}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);vertical-align:top}th{width:30%;font-weight:600;color:var(--mut)}\n</style></head><body><main>\n");
    h.push_str(&format!("<h1>Relatório do sistema</h1><p class=\"mut\">Gerado em {}</p>\n", html_escape(&format_local(rep.generated_at, "%d/%m/%Y %H:%M"))));
    h.push_str("<h2>Sistema</h2><table>\n");
    h.push_str(&row("Sistema", &format!("{} ({})", rep.os.pretty_name, rep.os.arch)));
    h.push_str(&row("Kernel", &rep.os.kernel));
    h.push_str(&row("Placa-mãe", format!("{} {}", rep.board.board_vendor, rep.board.board_name).trim()));
    h.push_str(&row("BIOS", format!("{} {} {}", rep.board.bios_vendor, rep.board.bios_version, rep.board.bios_date).trim()));
    h.push_str(&row("CPU", &format!("{} — {} núcleos / {} threads", rep.cpu.model, rep.cpu.cores, rep.cpu.threads)));
    h.push_str(&row("Memória", &format!("{} total, {} disponível", fmt_bytes(rep.memory.total), fmt_bytes(rep.memory.available))));
    for m in &rep.memory.modules {
        h.push_str(&row(&m.locator, &format!("{} MiB {} {} {} {}", m.size_mib, m.kind, opt(&m.configured_mts.or(m.speed_mts), " MT/s"), m.manufacturer, m.part_number)));
    }
    h.push_str("</table>\n");
    if !rep.gpus.is_empty() {
        h.push_str("<h2>Placas de vídeo</h2><table>\n");
        for g in &rep.gpus {
            h.push_str(&row(&g.name, &format!("driver {} · VRAM {} · temp {} · consumo {}", opt(&g.driver_version, ""), g.vram_total.map(fmt_bytes).unwrap_or_else(|| "—".into()), opt(&g.temp, " °C"), opt(&g.power_w, " W"))));
        }
        h.push_str("</table>\n");
    }
    if !rep.disks.is_empty() {
        h.push_str("<h2>Discos</h2><table>\n");
        for d in &rep.disks {
            let mut v = format!("{} · {} · {} · temp {} · {}", fmt_bytes(d.size), d.kind(), d.bus, opt(&d.temp, " °C"), d.verdict.label());
            for r in &d.reasons {
                v.push_str(&format!(" · {r}"));
            }
            h.push_str(&row(&format!("{} {}", d.name, d.title()), &v));
        }
        h.push_str("</table>\n");
    }
    if !rep.power.batteries.is_empty() {
        h.push_str("<h2>Bateria</h2><table>\n");
        for b in &rep.power.batteries {
            h.push_str(&row(&b.name, &format!("{} · {} · saúde {} · {} ciclos", opt(&b.capacity.map(|v| format!("{v:.0}")), " %"), b.status, opt(&b.health.map(|v| format!("{v:.0}")), " %"), opt(&b.cycles, ""))));
        }
        h.push_str("</table>\n");
    }
    if !rep.bench_records.is_empty() {
        h.push_str("<h2>Benchmarks</h2><table>\n");
        for b in &rep.bench_records {
            h.push_str(&row(&format!("{} · {}", b.test, format_local(b.date, "%d/%m/%Y %H:%M")), &format!("nota {:.0} — {}", b.score, b.summary)));
        }
        h.push_str("</table>\n");
    }
    if !rep.oc_history.is_empty() {
        h.push_str("<h2>Verificações de estabilidade</h2><table>\n");
        for o in &rep.oc_history {
            h.push_str(&row(&format!("{} · {:.0} min", format_local(o.date, "%d/%m/%Y %H:%M"), o.minutes), &format!("{} — {}", o.verdict, o.summary)));
        }
        h.push_str("</table>\n");
    }
    h.push_str("</main></body></html>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{BenchRecord, OcRecord};

    #[test]
    fn html_escapes_untrusted_text_and_has_no_scripts() {
        let mut s = Settings::default();
        s.bench_records.push(BenchRecord { date: 1_700_000_000, test: "<script>alert(1)</script>".into(), score: 123.4, summary: "a & b".into() });
        s.oc_history.push(OcRecord { date: 1_700_000_000, target: "cpu".into(), minutes: 10.0, verdict: "Estável".into(), summary: "\"ok\"".into(), max_cpu_temp: None, max_gpu_temp: None });
        let d = tempfile::tempdir().unwrap();
        let rep = collect(&Roots::under(d.path()), &s, false);
        let html = to_html(&rep);
        assert!(!html.contains("<script>"), "texto do usuário não pode virar tag");
        assert!(html.contains("&lt;script&gt;") && html.contains("a &amp; b"));
        assert!(html.contains("Verificações de estabilidade") && html.contains("nota 123"));
        assert!(!html.contains("http://") && !html.contains("https://"), "sem recursos externos");
        assert!(html.starts_with("<!doctype html>") && html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn empty_system_still_produces_valid_json_and_html() {
        let d = tempfile::tempdir().unwrap();
        let rep = collect(&Roots::under(d.path()), &Settings::default(), false);
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&rep).unwrap()).unwrap();
        assert!(v.get("os").is_some() && v["gpus"].as_array().unwrap().is_empty());
        assert!(!to_html(&rep).contains("<h2>Discos</h2>"));
    }
}
