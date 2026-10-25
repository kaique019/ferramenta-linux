//! RESET BENCH · simulação de jogo (CPU): o trabalho de CPU de um frame de jogo com 30 mil unidades em dois times.
//!
//! Por frame: (1) a thread principal monta a grade espacial (em série, como a maioria das engines);
//! (2) os workers (rayon) fazem física, colisão e IA em paralelo: cada unidade procura inimigos próximos, persegue ou
//! foge e troca dano; (3) a thread principal roda a lógica de jogo (mortes e renascimento) e prepara a lista de desenho
//! (culling + ordenação por profundidade). Determinístico: semente fixa e estado em buffer duplo, então o resultado
//! da simulação é idêntico em qualquer máquina (só o tempo muda).
//!
//! Nota = FPS médio da simulação, isto é, quantos frames de jogo por segundo a CPU aguenta.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::util::Cancel;

/// Unidades do teste oficial.
pub const ENTITIES: usize = 30_000;
pub const WORLD: f32 = 1000.0;
const GRID_N: usize = 64;
const CELL: f32 = WORLD / GRID_N as f32;
/// Raio de percepção das unidades.
const RADIUS: f32 = CELL;
const DT: f32 = 1.0 / 60.0;
/// Aquecimento (clock da CPU subindo, caches) fora da medição.
pub const WARMUP: Duration = Duration::from_secs(2);

/// Colunas e linhas do mapa de densidade que a interface desenha.
pub const DENSITY_W: usize = 48;
pub const DENSITY_H: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct SimResult {
    pub avg_fps: f64,
    /// 1% low: FPS correspondente ao percentil 99 do tempo de frame.
    pub low1_fps: f64,
    /// Tempo médio por frame gasto em trechos seriais (grade, lógica, desenho).
    pub main_ms: f64,
    /// Tempo médio por frame gasto na parte paralela (física, colisão, IA).
    pub parallel_ms: f64,
    pub frames: u64,
    pub seconds: f64,
    pub threads: usize,
    pub entities: usize,
}

impl SimResult {
    /// Nota = FPS médio da simulação.
    pub fn score(&self) -> f64 {
        self.avg_fps
    }
}

/// Estado compartilhado que a interface lê enquanto o teste roda.
pub struct SimProgress {
    pub frames: AtomicU64,
    /// Progresso da medição, de 0 a 1000.
    permille: AtomicU32,
    fps_bits: AtomicU64,
    /// Unidades dos times 0 e 1 em cada célula do mapa (`DENSITY_W` x `DENSITY_H`).
    density: Mutex<Vec<(u16, u16)>>,
}

impl SimProgress {
    pub fn new() -> Arc<SimProgress> {
        Arc::new(SimProgress { frames: AtomicU64::new(0), permille: AtomicU32::new(0), fps_bits: AtomicU64::new(0f64.to_bits()), density: Mutex::new(vec![(0, 0); DENSITY_W * DENSITY_H]) })
    }

    pub fn fraction(&self) -> f64 {
        self.permille.load(Relaxed) as f64 / 1000.0
    }

    /// FPS medido na última janela de meio segundo.
    pub fn current_fps(&self) -> f64 {
        f64::from_bits(self.fps_bits.load(Relaxed))
    }

    pub fn density(&self) -> Vec<(u16, u16)> {
        self.density.lock().map(|d| d.clone()).unwrap_or_default()
    }
}

/// Parte do estado que a física lê (somente leitura, compartilhada entre os workers).
struct View<'a> {
    x: &'a [f32],
    y: &'a [f32],
    vx: &'a [f32],
    vy: &'a [f32],
    hp: &'a [f32],
    team: &'a [u8],
    cell_of: &'a [u32],
    cell_start: &'a [u32],
    sorted: &'a [u32],
}

/// Física, separação (colisão) e IA de combate de uma unidade. Lê o estado atual e devolve só o da própria unidade:
/// (x, y, vx, vy, dano recebido neste frame).
fn simulate(v: &View, i: usize) -> (f32, f32, f32, f32, f32) {
    let (mut x, mut y, mut vx, mut vy) = (v.x[i], v.y[i], v.vx[i], v.vy[i]);
    let team = v.team[i];
    let (mut sep_x, mut sep_y, mut ali_x, mut ali_y, mut coh_x, mut coh_y, mut dmg) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    let mut best_d2 = RADIUS * RADIUS;
    let (mut enemy_x, mut enemy_y) = (0f32, 0f32);
    let mut allies = 0u32;
    let mut enemy = false;

    let c = v.cell_of[i] as usize;
    let (cx, cy) = (c % GRID_N, c / GRID_N);
    for gy in cy.saturating_sub(1)..=(cy + 1).min(GRID_N - 1) {
        for gx in cx.saturating_sub(1)..=(cx + 1).min(GRID_N - 1) {
            let cell = gy * GRID_N + gx;
            for k in v.cell_start[cell]..v.cell_start[cell + 1] {
                let j = v.sorted[k as usize] as usize;
                if j == i {
                    continue;
                }
                let (dx, dy) = (v.x[j] - x, v.y[j] - y);
                let d2 = dx * dx + dy * dy;
                if !(1e-6..=RADIUS * RADIUS).contains(&d2) {
                    continue;
                }
                let inv = 1.0 / d2.sqrt();
                if d2 < 9.0 {
                    // colisão: empurra
                    sep_x -= dx * inv;
                    sep_y -= dy * inv;
                }
                if v.team[j] == team {
                    ali_x += v.vx[j];
                    ali_y += v.vy[j];
                    coh_x += dx;
                    coh_y += dy;
                    allies += 1;
                } else {
                    if d2 < 16.0 {
                        // corpo a corpo
                        dmg += 4.0 * DT * (1.0 - d2.sqrt() / 4.0);
                    }
                    if d2 < best_d2 {
                        best_d2 = d2;
                        enemy_x = dx;
                        enemy_y = dy;
                        enemy = true;
                    }
                }
            }
        }
    }

    // Decisão de IA: vida baixa -> foge do inimigo mais próximo; senão persegue.
    let (mut ax, mut ay) = (sep_x * 60.0, sep_y * 60.0);
    if allies > 0 {
        let n = allies as f32;
        ax += (ali_x / n - vx) * 0.5 + coh_x / n * 0.3;
        ay += (ali_y / n - vy) * 0.5 + coh_y / n * 0.3;
    }
    if enemy {
        let inv = 1.0 / best_d2.sqrt();
        let dir = if v.hp[i] < 30.0 { -1.0 } else { 1.0 };
        ax += dir * enemy_x * inv * 80.0;
        ay += dir * enemy_y * inv * 80.0;
    } else {
        // sem inimigo por perto: marcha para a base inimiga
        ax += if team == 0 { 25.0 } else { -25.0 };
    }

    vx += ax * DT;
    vy += ay * DT;
    let sp = (vx * vx + vy * vy).sqrt();
    if sp > 60.0 {
        vx *= 60.0 / sp;
        vy *= 60.0 / sp;
    }
    x += vx * DT;
    y += vy * DT;
    if !(0.0..=WORLD).contains(&x) {
        vx = -vx;
        x = x.clamp(0.0, WORLD);
    }
    if !(0.0..=WORLD).contains(&y) {
        vy = -vy;
        y = y.clamp(0.0, WORLD);
    }
    (x, y, vx, vy, dmg)
}

/// A simulação. Estado em arrays separados (SoA) com buffer duplo: lê de `x/y/vx/vy` e escreve em `nx/ny/nvx/nvy`.
pub struct Sim {
    n: usize,
    x: Vec<f32>,
    y: Vec<f32>,
    vx: Vec<f32>,
    vy: Vec<f32>,
    nx: Vec<f32>,
    ny: Vec<f32>,
    nvx: Vec<f32>,
    nvy: Vec<f32>,
    hp: Vec<f32>,
    dmg: Vec<f32>,
    team: Vec<u8>,
    cell_start: Vec<u32>,
    sorted: Vec<u32>,
    cell_of: Vec<u32>,
    draw: Vec<(f32, u32)>,
    rng: u32,
    tick: u64,
}

impl Sim {
    pub fn new() -> Sim {
        Self::with_entities(ENTITIES)
    }

    /// Simulação com outro número de unidades (usado nos testes).
    pub fn with_entities(n: usize) -> Sim {
        let mut s = Sim {
            n,
            x: vec![0.0; n],
            y: vec![0.0; n],
            vx: vec![0.0; n],
            vy: vec![0.0; n],
            nx: vec![0.0; n],
            ny: vec![0.0; n],
            nvx: vec![0.0; n],
            nvy: vec![0.0; n],
            hp: vec![100.0; n],
            dmg: vec![0.0; n],
            team: vec![0; n],
            cell_start: vec![0; GRID_N * GRID_N + 1],
            sorted: vec![0; n],
            cell_of: vec![0; n],
            draw: Vec::with_capacity(n),
            rng: 12345,
            tick: 0,
        };
        s.reset();
        s
    }

    pub fn entities(&self) -> usize {
        self.n
    }

    pub fn positions(&self) -> (&[f32], &[f32]) {
        (&self.x, &self.y)
    }

    pub fn teams(&self) -> &[u8] {
        &self.team
    }

    pub fn health(&self) -> &[f32] {
        &self.hp
    }

    fn rnd(&mut self) -> f32 {
        // xorshift32: rápido e determinístico
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    fn reset(&mut self) {
        self.rng = 12345;
        self.tick = 0;
        for i in 0..self.n {
            self.team[i] = (i & 1) as u8;
            let t0 = self.team[i] == 0;
            // Times começam em lados opostos e avançam um contra o outro.
            self.x[i] = (if t0 { 0.05 } else { 0.55 }) * WORLD + self.rnd() * WORLD * 0.4;
            self.y[i] = self.rnd() * WORLD;
            self.vx[i] = (if t0 { 1.0 } else { -1.0 }) * (20.0 + self.rnd() * 10.0);
            self.vy[i] = (self.rnd() - 0.5) * 10.0;
            self.hp[i] = 100.0;
            self.dmg[i] = 0.0;
        }
    }

    /// Counting sort das unidades por célula (grade espacial para buscar vizinhos).
    fn build_grid(&mut self) {
        self.cell_start.fill(0);
        for i in 0..self.n {
            let cx = ((self.x[i] / CELL) as usize).min(GRID_N - 1);
            let cy = ((self.y[i] / CELL) as usize).min(GRID_N - 1);
            let c = cy * GRID_N + cx;
            self.cell_of[i] = c as u32;
            self.cell_start[c + 1] += 1;
        }
        for c in 0..GRID_N * GRID_N {
            self.cell_start[c + 1] += self.cell_start[c];
        }
        let mut fill = self.cell_start.clone();
        for i in 0..self.n {
            let c = self.cell_of[i] as usize;
            self.sorted[fill[c] as usize] = i as u32;
            fill[c] += 1;
        }
    }

    /// Lógica de jogo em série: aplica dano; quem morre ou chega à base inimiga ("pontua") renasce na própria base.
    /// O fluxo contínuo nos dois sentidos mantém uma frente de batalha no meio e a carga estável durante o teste.
    fn game_logic(&mut self) {
        for i in 0..self.n {
            self.hp[i] = (self.hp[i] - self.dmg[i] + 0.5 * DT).min(100.0);
            let t0 = self.team[i] == 0;
            let scored = if t0 { self.x[i] > WORLD * 0.97 } else { self.x[i] < WORLD * 0.03 };
            if self.hp[i] > 0.0 && !scored {
                continue;
            }
            self.hp[i] = 100.0;
            self.x[i] = (if t0 { 0.02 } else { 0.98 }) * WORLD;
            self.y[i] = self.rnd() * WORLD;
            self.vx[i] = if t0 { 25.0 } else { -25.0 };
            self.vy[i] = 0.0;
        }
    }

    /// Preparação do desenho em série: câmera em perspectiva, culling e ordenação de trás para frente.
    fn prepare_draw(&mut self) {
        let cam_x = WORLD / 2.0 + (self.tick as f32 * 0.01).sin() * 200.0;
        let cam_y = -200.0f32;
        self.draw.clear();
        for i in 0..self.n {
            let (dx, dy) = (self.x[i] - cam_x, self.y[i] - cam_y);
            if dy <= 0.0 || dx.abs() > dy * 1.2 {
                continue; // fora do campo de visão
            }
            self.draw.push((-dy, i as u32)); // mais longe primeiro
        }
        self.draw.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    }

    /// Quantas unidades passaram pelo culling no último frame.
    pub fn visible(&self) -> usize {
        self.draw.len()
    }

    /// Avança um frame. Devolve (ms em trechos seriais, ms na parte paralela).
    pub fn step(&mut self) -> (f64, f64) {
        let t0 = Instant::now();
        self.build_grid(); // série
        let t1 = Instant::now();
        {
            let view = View { x: &self.x, y: &self.y, vx: &self.vx, vy: &self.vy, hp: &self.hp, team: &self.team, cell_of: &self.cell_of, cell_start: &self.cell_start, sorted: &self.sorted };
            self.nx
                .par_iter_mut()
                .zip(self.ny.par_iter_mut())
                .zip(self.nvx.par_iter_mut())
                .zip(self.nvy.par_iter_mut())
                .zip(self.dmg.par_iter_mut())
                .enumerate()
                .with_min_len(256)
                .for_each(|(i, ((((nx, ny), nvx), nvy), dmg))| {
                    let (x, y, vx, vy, d) = simulate(&view, i);
                    *nx = x;
                    *ny = y;
                    *nvx = vx;
                    *nvy = vy;
                    *dmg = d;
                }); // paralelo
        }
        let t2 = Instant::now();
        std::mem::swap(&mut self.x, &mut self.nx);
        std::mem::swap(&mut self.y, &mut self.ny);
        std::mem::swap(&mut self.vx, &mut self.nvx);
        std::mem::swap(&mut self.vy, &mut self.nvy);
        self.game_logic(); // série
        self.prepare_draw(); // série
        self.tick += 1;
        let t3 = Instant::now();
        let main = t1.duration_since(t0) + t3.duration_since(t2);
        (main.as_secs_f64() * 1000.0, t2.duration_since(t1).as_secs_f64() * 1000.0)
    }

    /// Unidades dos dois times em cada célula de um mapa `DENSITY_W` x `DENSITY_H`.
    pub fn density(&self) -> Vec<(u16, u16)> {
        let mut d = vec![(0u16, 0u16); DENSITY_W * DENSITY_H];
        for i in 0..self.n {
            let cx = ((self.x[i] / WORLD * DENSITY_W as f32) as usize).min(DENSITY_W - 1);
            let cy = ((self.y[i] / WORLD * DENSITY_H as f32) as usize).min(DENSITY_H - 1);
            let cell = &mut d[cy * DENSITY_W + cx];
            if self.team[i] == 0 { cell.0 = cell.0.saturating_add(1) } else { cell.1 = cell.1.saturating_add(1) }
        }
        d
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

/// 1% low: FPS correspondente ao percentil 99 do tempo de frame (em ms). Ordena `frame_ms`.
pub fn low1_fps(frame_ms: &mut [f64]) -> f64 {
    if frame_ms.is_empty() {
        return 0.0;
    }
    frame_ms.sort_by(|a, b| a.total_cmp(b));
    let idx = ((frame_ms.len() as f64 * 0.99) as usize).min(frame_ms.len() - 1);
    let p99 = frame_ms[idx];
    if p99 > 0.0 { 1000.0 / p99 } else { 0.0 }
}

/// Executa o teste com `entities` unidades. `warmup` roda antes e não entra na medição.
pub fn run_with(entities: usize, warmup: Duration, duration: Duration, cancel: &Cancel, progress: &Arc<SimProgress>) -> SimResult {
    let mut sim = Sim::with_entities(entities);
    let threads = rayon::current_num_threads();
    progress.frames.store(0, Relaxed);
    progress.permille.store(0, Relaxed);

    let warm = Instant::now();
    while warm.elapsed() < warmup && !cancel.is_cancelled() {
        sim.step();
    }

    let mut frame_ms: Vec<f64> = Vec::with_capacity(20_000);
    let (mut main_sum, mut par_sum) = (0.0f64, 0.0f64);
    let start = Instant::now();
    let mut window = Instant::now();
    let mut window_frames = 0u32;
    while start.elapsed() < duration && !cancel.is_cancelled() {
        let t = Instant::now();
        let (m, p) = sim.step();
        frame_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        main_sum += m;
        par_sum += p;
        progress.frames.fetch_add(1, Relaxed);
        window_frames += 1;
        if window.elapsed() >= Duration::from_millis(500) {
            progress.fps_bits.store((window_frames as f64 / window.elapsed().as_secs_f64()).to_bits(), Relaxed);
            if let Ok(mut d) = progress.density.lock() {
                *d = sim.density();
            }
            window_frames = 0;
            window = Instant::now();
        }
        progress.permille.store(((start.elapsed().as_secs_f64() / duration.as_secs_f64().max(1e-9)).min(1.0) * 1000.0) as u32, Relaxed);
    }
    let seconds = start.elapsed().as_secs_f64();
    let frames = frame_ms.len() as u64;
    if !cancel.is_cancelled() {
        progress.permille.store(1000, Relaxed);
    }
    let n = frames.max(1) as f64;
    SimResult {
        avg_fps: if seconds > 0.0 { frames as f64 / seconds } else { 0.0 },
        low1_fps: low1_fps(&mut frame_ms),
        main_ms: main_sum / n,
        parallel_ms: par_sum / n,
        frames,
        seconds,
        threads,
        entities,
    }
}

/// Teste oficial: 30 mil unidades, 2 s de aquecimento e `duration` de medição.
pub fn run(duration: Duration, cancel: &Cancel, progress: &Arc<SimProgress>) -> SimResult {
    run_with(ENTITIES, WARMUP, duration, cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advance(n: usize, steps: usize) -> Sim {
        let mut s = Sim::with_entities(n);
        for _ in 0..steps {
            s.step();
        }
        s
    }

    fn mean_x(s: &Sim, team: u8) -> f64 {
        let (x, _) = s.positions();
        let v: Vec<f64> = (0..s.entities()).filter(|&i| s.teams()[i] == team).map(|i| x[i] as f64).collect();
        v.iter().sum::<f64>() / v.len() as f64
    }

    #[test]
    fn simulation_is_deterministic() {
        let (a, b) = (advance(3000, 40), advance(3000, 40));
        assert_eq!(a.positions().0, b.positions().0);
        assert_eq!(a.positions().1, b.positions().1);
        assert_eq!(a.health(), b.health());
    }

    #[test]
    fn grid_holds_every_unit_exactly_once_in_the_right_cell() {
        let mut s = advance(2000, 5);
        s.build_grid();
        assert_eq!(*s.cell_start.last().unwrap() as usize, s.n);
        assert!(s.cell_start.windows(2).all(|w| w[0] <= w[1]));
        let mut seen = vec![false; s.n];
        for c in 0..GRID_N * GRID_N {
            for k in s.cell_start[c]..s.cell_start[c + 1] {
                let i = s.sorted[k as usize] as usize;
                assert!(!seen[i], "unidade {i} repetida");
                seen[i] = true;
                assert_eq!(s.cell_of[i] as usize, c);
            }
        }
        assert!(seen.iter().all(|v| *v));
    }

    #[test]
    fn units_stay_inside_the_world_and_hp_stays_valid() {
        let s = advance(2500, 150);
        let (x, y) = s.positions();
        assert!(x.iter().chain(y.iter()).all(|v| v.is_finite() && (0.0..=WORLD).contains(v)));
        assert!(s.health().iter().all(|h| *h > 0.0 && *h <= 100.0));
    }

    #[test]
    fn teams_advance_toward_each_other() {
        let before = Sim::with_entities(3000);
        let after = advance(3000, 120);
        assert!(mean_x(&after, 0) > mean_x(&before, 0) + 30.0, "time 0 deve avançar para a direita");
        assert!(mean_x(&after, 1) < mean_x(&before, 1) - 30.0, "time 1 deve avançar para a esquerda");
    }

    #[test]
    fn culling_keeps_only_units_in_view() {
        let s = advance(2000, 3);
        assert!(s.visible() > 0 && s.visible() < s.entities());
    }

    #[test]
    fn density_map_counts_all_units() {
        let s = advance(1000, 10);
        let d = s.density();
        assert_eq!(d.len(), DENSITY_W * DENSITY_H);
        let total: usize = d.iter().map(|(a, b)| *a as usize + *b as usize).sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn low1_uses_the_99th_percentile() {
        let mut v: Vec<f64> = vec![10.0; 99];
        v.push(50.0);
        // 100 frames: índice 99 = o mais lento (50 ms) => 20 FPS
        assert!((low1_fps(&mut v) - 20.0).abs() < 1e-9);
        let mut same = vec![8.0; 200];
        assert!((low1_fps(&mut same) - 125.0).abs() < 1e-9);
        assert_eq!(low1_fps(&mut []), 0.0);
    }

    #[test]
    fn short_run_reports_consistent_numbers() {
        let progress = SimProgress::new();
        let r = run_with(2000, Duration::from_millis(50), Duration::from_millis(400), &Cancel::new(), &progress);
        assert!(r.frames > 5, "frames = {}", r.frames);
        assert!(r.avg_fps > 0.0 && r.low1_fps > 0.0 && r.avg_fps.is_finite());
        assert!(r.main_ms >= 0.0 && r.parallel_ms >= 0.0);
        assert_eq!(r.entities, 2000);
        assert_eq!(r.score(), r.avg_fps);
        assert!((progress.fraction() - 1.0).abs() < 1e-9);
        assert_eq!(progress.frames.load(Relaxed), r.frames);
    }

    #[test]
    fn cancelled_run_returns_without_nan() {
        let c = Cancel::new();
        c.cancel();
        let r = run_with(1000, Duration::from_secs(1), Duration::from_secs(5), &c, &SimProgress::new());
        assert_eq!(r.frames, 0);
        assert_eq!(r.avg_fps, 0.0);
        assert!(!r.main_ms.is_nan() && !r.parallel_ms.is_nan());
    }
}
