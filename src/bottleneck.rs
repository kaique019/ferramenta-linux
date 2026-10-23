//! Detector de gargalo: classifica o que está limitando o FPS a partir de leituras de sensores (1 por segundo)
//! e resume uma sessão de jogo. Porta da lógica do `BottleneckAnalyzer` do projeto Windows.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::gpu::GpuInfo;
use crate::sensors::CpuLoad;
use crate::sysinfo::MemInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Limiter {
    Gpu,
    Cpu,
    Vram,
    Ram,
    Thermal,
    Capped,
}

impl Limiter {
    pub fn title(self) -> &'static str {
        match self {
            Limiter::Gpu => "GPU no limite",
            Limiter::Cpu => "Gargalo de CPU",
            Limiter::Vram => "Memória de vídeo cheia",
            Limiter::Ram => "Memória RAM cheia",
            Limiter::Thermal => "Limitado por temperatura",
            Limiter::Capped => "Nada no limite",
        }
    }

    pub fn advice(self) -> &'static str {
        match self {
            Limiter::Gpu => "Normal em jogos pesados: a placa de vídeo está sendo bem aproveitada. Para mais FPS, reduza resolução ou qualidade gráfica, ou ative DLSS/FSR.",
            Limiter::Cpu => "A CPU (ou um núcleo dela) não acompanha a GPU. Subir resolução/gráficos quase não custa FPS. Para ganhar FPS: reduza opções que pesam na CPU (distância de visão, multidões, física), feche programas em segundo plano, use o governador de CPU \"performance\" (ou o GameMode) e confira se o XMP/EXPO da RAM está ativo na BIOS.",
            Limiter::Vram => "A memória da placa de vídeo encheu, o que causa travadas. Reduza a qualidade das texturas ou a resolução.",
            Limiter::Ram => "Pouca RAM livre: o sistema passa a usar swap (disco) e o jogo engasga. Feche navegadores e outros programas.",
            Limiter::Thermal => "Temperatura alta está derrubando o clock. Verifique ventilação do gabinete, poeira e pasta térmica.",
            Limiter::Capped => "Nem CPU nem GPU estão no limite: provavelmente há trava de FPS, V-Sync/G-Sync limitando, ou o jogo está em menu/pausa. Normal se o FPS já é o desejado.",
        }
    }
}

/// Uma leitura de tudo que pode limitar o FPS.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LoadSample {
    pub gpu_load: Option<f64>,
    pub cpu_total: Option<f64>,
    pub cpu_max_core: Option<f64>,
    pub busiest_core: Option<usize>,
    pub vram_pct: Option<f64>,
    pub ram_pct: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub cpu_temp: Option<f64>,
    pub gpu_thermal_cap: bool,
    pub fps: Option<f64>,
}

fn ge(v: Option<f64>, limit: f64) -> bool {
    v.is_some_and(|x| x >= limit)
}

impl LoadSample {
    /// Regras, na ordem: VRAM cheia → térmico → GPU ≥ 90% → CPU/núcleo ≥ 85% com GPU sobrando →
    /// RAM cheia → nada no limite (trava de FPS, V-Sync ou menu).
    pub fn classify(&self) -> Limiter {
        if ge(self.vram_pct, 97.0) {
            Limiter::Vram
        } else if self.gpu_thermal_cap || ge(self.cpu_temp, 95.0) {
            Limiter::Thermal
        } else if ge(self.gpu_load, 90.0) {
            Limiter::Gpu
        } else if ge(self.cpu_max_core, 85.0) || ge(self.cpu_total, 85.0) {
            Limiter::Cpu
        } else if ge(self.ram_pct, 92.0) {
            Limiter::Ram
        } else {
            Limiter::Capped
        }
    }

    /// Monta a leitura a partir dos coletores do programa.
    pub fn from_parts(cpu: &CpuLoad, cpu_temp: Option<f64>, gpu: Option<&GpuInfo>, mem: &MemInfo, fps: Option<f64>) -> Self {
        let busiest = cpu.busiest();
        LoadSample {
            gpu_load: gpu.and_then(|g| g.util_gpu),
            cpu_total: Some(cpu.total),
            cpu_max_core: busiest.map(|(_, v)| v),
            busiest_core: busiest.map(|(i, _)| i),
            vram_pct: gpu.and_then(|g| g.vram_pct()),
            ram_pct: (mem.total > 0).then(|| mem.used() as f64 / mem.total as f64 * 100.0),
            gpu_temp: gpu.and_then(|g| g.temp),
            cpu_temp,
            gpu_thermal_cap: gpu.is_some_and(|g| g.thermal_throttle),
            fps,
        }
    }
}

/// Equilíbrio teórico pelo desempenho relativo (GPU: RTX 4070 = 100; CPU: Ryzen 7 7700X = 100).
pub fn balance(gpu_idx: f64, cpu_idx: f64) -> (f64, &'static str) {
    let ratio = gpu_idx / cpu_idx;
    let text = if ratio > 1.8 {
        "GPU muito mais forte que a CPU: em boa parte dos jogos a CPU vai limitar o FPS, principalmente em 1080p."
    } else if ratio > 1.3 {
        "GPU um pouco à frente da CPU: jogos competitivos em 1080p tendem a ficar limitados pela CPU; em 1440p/4K a GPU volta a ser o limite."
    } else if ratio >= 0.7 {
        "Hardware equilibrado: na maioria dos jogos a GPU será o limite, que é o cenário ideal."
    } else if ratio > 0.4 {
        "CPU mais forte que a GPU: a placa de vídeo é o limite. Upgrade de GPU traria o maior ganho."
    } else {
        "GPU muito fraca para esta CPU: a placa de vídeo limita quase tudo."
    };
    (ratio, text)
}

/// Acumula amostras durante uma sessão e produz o relatório.
#[derive(Debug)]
pub struct Session {
    samples: Vec<(LoadSample, Limiter)>,
    start: Instant,
    duration: Duration,
}

impl Session {
    pub fn new(duration: Duration) -> Self {
        Session { samples: Vec::new(), start: Instant::now(), duration }
    }

    pub fn add(&mut self, s: LoadSample) {
        let l = s.classify();
        self.samples.push((s, l));
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn done(&self) -> bool {
        self.start.elapsed() >= self.duration
    }

    pub fn progress(&self) -> f64 {
        (self.start.elapsed().as_secs_f64() / self.duration.as_secs_f64().max(0.001)).min(1.0)
    }

    /// Porcentagem do tempo em cada limitador, do maior para o menor.
    pub fn distribution(&self) -> Vec<(Limiter, f64)> {
        let n = self.samples.len().max(1) as f64;
        let mut counts: Vec<(Limiter, usize)> = Vec::new();
        for (_, l) in &self.samples {
            match counts.iter_mut().find(|(k, _)| k == l) {
                Some(e) => e.1 += 1,
                None => counts.push((*l, 1)),
            }
        }
        let mut v: Vec<(Limiter, f64)> = counts.into_iter().map(|(l, c)| (l, c as f64 * 100.0 / n)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    }

    /// Limitador principal da sessão.
    pub fn main(&self) -> Option<Limiter> {
        self.distribution().first().map(|(l, _)| *l)
    }

    pub fn avg_fps(&self) -> Option<f64> {
        self.avg(|s| s.fps.filter(|f| *f > 0.0))
    }

    pub fn avg(&self, sel: impl Fn(&LoadSample) -> Option<f64>) -> Option<f64> {
        let v: Vec<f64> = self.samples.iter().filter_map(|(s, _)| sel(s)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }

    pub fn max(&self, sel: impl Fn(&LoadSample) -> Option<f64>) -> Option<f64> {
        self.samples.iter().filter_map(|(s, _)| sel(s)).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))))
    }

    /// Recomendações específicas da sessão (além do conselho do limitador principal).
    pub fn recommendations(&self) -> Vec<String> {
        let mut r = Vec::new();
        let Some(main) = self.main() else { return r };
        r.push(main.advice().to_string());
        for (l, pct) in self.distribution().into_iter().skip(1) {
            if pct >= 20.0 && l != Limiter::Capped {
                r.push(format!("{:.0}% do tempo: {} — {}", pct, l.title().to_lowercase(), l.advice()));
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> LoadSample {
        LoadSample::default()
    }

    #[test]
    fn classification_follows_the_documented_order() {
        assert_eq!(s().classify(), Limiter::Capped);
        // VRAM vence tudo
        let a = LoadSample { vram_pct: Some(97.0), gpu_load: Some(99.0), gpu_thermal_cap: true, ..s() };
        assert_eq!(a.classify(), Limiter::Vram);
        // térmico vence GPU e CPU
        let b = LoadSample { gpu_thermal_cap: true, gpu_load: Some(99.0), cpu_total: Some(99.0), ..s() };
        assert_eq!(b.classify(), Limiter::Thermal);
        let c = LoadSample { cpu_temp: Some(95.0), ..s() };
        assert_eq!(c.classify(), Limiter::Thermal);
        // GPU vence CPU
        let d = LoadSample { gpu_load: Some(90.0), cpu_max_core: Some(100.0), ..s() };
        assert_eq!(d.classify(), Limiter::Gpu);
        // CPU por núcleo ou total
        assert_eq!(LoadSample { gpu_load: Some(60.0), cpu_max_core: Some(85.0), ..s() }.classify(), Limiter::Cpu);
        assert_eq!(LoadSample { cpu_total: Some(85.0), ..s() }.classify(), Limiter::Cpu);
        // RAM só se nada acima
        assert_eq!(LoadSample { ram_pct: Some(92.0), cpu_total: Some(50.0), ..s() }.classify(), Limiter::Ram);
        assert_eq!(LoadSample { ram_pct: Some(91.9), ..s() }.classify(), Limiter::Capped);
    }

    #[test]
    fn missing_sensors_never_trigger_a_limiter() {
        assert_eq!(s().classify(), Limiter::Capped);
        assert_eq!(LoadSample { gpu_load: Some(89.9), cpu_total: Some(84.9), cpu_max_core: Some(84.9), ..s() }.classify(), Limiter::Capped);
    }

    #[test]
    fn balance_thresholds() {
        assert!(balance(200.0, 100.0).1.contains("CPU vai limitar"));
        assert!(balance(150.0, 100.0).1.contains("um pouco à frente"));
        assert!(balance(100.0, 100.0).1.contains("equilibrado"));
        assert!(balance(70.0, 100.0).1.contains("equilibrado"));
        assert!(balance(50.0, 100.0).1.contains("CPU mais forte"));
        assert!(balance(30.0, 100.0).1.contains("muito fraca"));
        assert!((balance(118.0, 100.0).0 - 1.18).abs() < 1e-9);
    }

    #[test]
    fn session_distribution_and_averages() {
        let mut se = Session::new(Duration::from_secs(60));
        for _ in 0..6 {
            se.add(LoadSample { gpu_load: Some(98.0), fps: Some(100.0), gpu_temp: Some(70.0), ..s() });
        }
        for _ in 0..3 {
            se.add(LoadSample { cpu_max_core: Some(95.0), fps: Some(80.0), gpu_temp: Some(60.0), ..s() });
        }
        se.add(LoadSample::default());
        assert_eq!(se.count(), 10);
        let d = se.distribution();
        assert_eq!(d[0].0, Limiter::Gpu);
        assert!((d[0].1 - 60.0).abs() < 1e-9);
        assert_eq!(d[1].0, Limiter::Cpu);
        assert!((d[1].1 - 30.0).abs() < 1e-9);
        assert_eq!(se.main(), Some(Limiter::Gpu));
        assert!((se.avg_fps().unwrap() - 840.0 / 9.0).abs() < 1e-9, "amostra sem FPS não entra na média");
        assert_eq!(se.max(|x| x.gpu_temp), Some(70.0));
        assert!(!se.done());
        let rec = se.recommendations();
        assert!(rec[0].contains("placa de vídeo"));
        assert!(rec.iter().any(|r| r.starts_with("30% do tempo")));
        assert_eq!(Session::new(Duration::from_secs(1)).main(), None);
    }

    #[test]
    fn builds_a_sample_from_collectors() {
        let cpu = CpuLoad { total: 40.0, per_core: vec![10.0, 92.0, 30.0] };
        let gpu = GpuInfo { util_gpu: Some(55.0), vram_total: Some(8 << 30), vram_used: Some(4 << 30), temp: Some(66.0), thermal_throttle: true, ..Default::default() };
        let mem = MemInfo { total: 16 << 30, available: 4 << 30, ..Default::default() };
        let x = LoadSample::from_parts(&cpu, Some(70.0), Some(&gpu), &mem, Some(144.0));
        assert_eq!(x.busiest_core, Some(1));
        assert_eq!(x.cpu_max_core, Some(92.0));
        assert_eq!(x.vram_pct, Some(50.0));
        assert_eq!(x.ram_pct, Some(75.0));
        assert!(x.gpu_thermal_cap);
        assert_eq!(x.classify(), Limiter::Thermal);
        let none = LoadSample::from_parts(&cpu, None, None, &mem, None);
        assert_eq!(none.gpu_load, None);
        assert_eq!(none.classify(), Limiter::Cpu);
    }
}
