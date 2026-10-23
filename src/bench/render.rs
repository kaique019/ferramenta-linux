//! RESET BENCH: benchmark de CPU no princípio do Cinebench. Path tracer monocromático (iluminação global, sombras
//! suaves, metal e vidro com refração), imagem dividida em blocos de 32 px distribuídos entre as threads do centro
//! para fora. Determinístico: cada pixel tem semente fixa, então imagem e volume de trabalho são idênticos em toda execução.
//!
//! Nota = amostras de path tracing por segundo ÷ 10.000 (mesma cena e mesma carga em qualquer máquina).

use std::ops::{Add, Mul, Neg, Sub};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::util::Cancel;

pub const WIDTH: usize = 960;
pub const HEIGHT: usize = 540;
pub const BUCKET: usize = 32;
/// Amostras por pixel do teste oficial.
pub const SPP: u32 = 1024;
const MAX_DEPTH: usize = 6;

/// Tamanho e qualidade da imagem. O padrão é o teste oficial; valores menores servem para testes rápidos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderParams {
    pub width: usize,
    pub height: usize,
    pub spp: u32,
}

impl RenderParams {
    pub const OFFICIAL: RenderParams = RenderParams { width: WIDTH, height: HEIGHT, spp: SPP };
}

impl Default for RenderParams {
    fn default() -> Self {
        Self::OFFICIAL
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchResult {
    pub threads: usize,
    pub seconds: f64,
    pub samples: u64,
    pub passes: u32,
}

impl BenchResult {
    pub fn score(&self) -> f64 {
        if self.seconds > 0.0 { self.samples as f64 / self.seconds / 10_000.0 } else { 0.0 }
    }
}

// ---------------------------------------------------------------------------------------------
// Vetores
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }
}
impl Neg for V3 {
    type Output = V3;
    fn neg(self) -> V3 {
        v3(-self.x, -self.y, -self.z)
    }
}

impl V3 {
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        v3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    fn len2(self) -> f32 {
        self.dot(self)
    }
    fn norm(self) -> V3 {
        self * (1.0 / self.len2().sqrt())
    }
    fn reflect(self, n: V3) -> V3 {
        self - n * (2.0 * self.dot(n))
    }
}

// ---------------------------------------------------------------------------------------------
// Cena
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mat {
    Diffuse,
    Metal,
    Glass,
}

#[derive(Clone, Copy)]
struct Sphere {
    c: V3,
    r: f32,
    m: Mat,
    albedo: f32,
    rough: f32,
}

const LAMP_C: V3 = v3(-4.0, 7.0, -3.0);
const LAMP_R: f32 = 2.2;
const LAMP_POWER: f32 = 9.0;

struct Scene {
    spheres: Vec<Sphere>,
    origin: V3,
    lower: V3,
    horizontal: V3,
    vertical: V3,
}

impl Scene {
    fn new(width: usize, height: usize) -> Scene {
        let sp = |c: V3, r: f32, m: Mat, albedo: f32, rough: f32| Sphere { c, r, m, albedo, rough };
        let mut spheres = vec![
            sp(v3(0.0, 1.1, 0.0), 1.1, Mat::Glass, 0.98, 0.0),
            sp(v3(-2.5, 0.9, 0.9), 0.9, Mat::Metal, 0.92, 0.03),
            sp(v3(2.4, 0.8, 0.7), 0.8, Mat::Diffuse, 0.85, 0.0),
            sp(v3(1.0, 0.45, -1.9), 0.45, Mat::Metal, 0.85, 0.25),
            sp(v3(-1.1, 0.35, -2.0), 0.35, Mat::Diffuse, 0.15, 0.0),
        ];
        // Anel de esferas pequenas alternando materiais
        for i in 0..14 {
            let a = i as f32 / 14.0 * std::f32::consts::TAU;
            let m = match i % 3 {
                0 => Mat::Metal,
                1 => Mat::Diffuse,
                _ => Mat::Glass,
            };
            spheres.push(sp(v3(a.cos() * 4.2, 0.28, a.sin() * 4.2 + 1.5), 0.28, m, if i % 2 == 0 { 0.9 } else { 0.3 }, 0.1));
        }
        let origin = v3(0.0, 2.1, -7.2);
        let target = v3(0.0, 0.75, 0.0);
        let vfov = 38.0f32 * std::f32::consts::PI / 180.0;
        let aspect = width as f32 / height as f32;
        let hh = (vfov / 2.0).tan();
        let hw = aspect * hh;
        let w = (origin - target).norm();
        let u = v3(0.0, 1.0, 0.0).cross(w).norm();
        let v = w.cross(u);
        Scene { spheres, origin, lower: origin - u * hw + v * hh - w, horizontal: u * (2.0 * hw), vertical: v * (-2.0 * hh) }
    }

    fn ray(&self, s: f32, t: f32) -> (V3, V3) {
        (self.origin, (self.lower + self.horizontal * s + self.vertical * t - self.origin).norm())
    }
}

fn rnd(s: &mut u32) -> f32 {
    // xorshift32: rápido e determinístico por pixel
    *s ^= *s << 13;
    *s ^= *s >> 17;
    *s ^= *s << 5;
    (*s >> 8) as f32 * (1.0 / 16_777_216.0)
}

fn random_in_sphere(s: &mut u32) -> V3 {
    loop {
        let v = v3(rnd(s) * 2.0 - 1.0, rnd(s) * 2.0 - 1.0, rnd(s) * 2.0 - 1.0);
        if v.len2() < 1.0 {
            return v;
        }
    }
}

fn basis(n: V3) -> (V3, V3) {
    let up = if n.x.abs() > 0.9 { v3(0.0, 1.0, 0.0) } else { v3(1.0, 0.0, 0.0) };
    let a = up.cross(n).norm();
    (a, n.cross(a))
}

fn cosine_hemisphere(n: V3, s: &mut u32) -> V3 {
    let (r1, r2) = (rnd(s), rnd(s));
    let (r, phi) = (r1.sqrt(), std::f32::consts::TAU * r2);
    let (a, b) = basis(n);
    (a * (r * phi.cos()) + b * (r * phi.sin()) + n * (1.0 - r1).sqrt()).norm()
}

fn hit_sphere(c: V3, r: f32, o: V3, d: V3, t_max: f32) -> Option<f32> {
    let oc = o - c;
    let b = oc.dot(d);
    let disc = b * b - (oc.len2() - r * r);
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let mut t = -b - sq;
    if t < 1e-4 {
        t = -b + sq;
    }
    (t > 1e-4 && t < t_max).then_some(t)
}

struct Hit {
    t: f32,
    /// -1 = chão, -2 = nada, >= 0 = esfera
    idx: i32,
    lamp: bool,
}

fn intersect(scene: &Scene, o: V3, d: V3) -> Option<Hit> {
    let mut hit = Hit { t: f32::MAX, idx: -2, lamp: false };
    if d.y < -1e-6 {
        let t = -o.y / d.y;
        if t > 1e-4 && t < 60.0 {
            hit.t = t;
            hit.idx = -1;
        }
    }
    for (i, s) in scene.spheres.iter().enumerate() {
        if let Some(t) = hit_sphere(s.c, s.r, o, d, hit.t) {
            hit.t = t;
            hit.idx = i as i32;
        }
    }
    if let Some(t) = hit_sphere(LAMP_C, LAMP_R, o, d, hit.t) {
        hit.t = t;
        hit.lamp = true;
    }
    (hit.idx != -2 || hit.lamp).then_some(hit)
}

fn sky(d: V3) -> f32 {
    0.06 + 0.22 * d.y.max(0.0)
}

/// Amostra uma direção dentro do cone que a lâmpada esférica ocupa e testa a sombra.
fn direct_light(scene: &Scene, p: V3, n: V3, seed: &mut u32) -> f32 {
    let to_c = LAMP_C - p;
    let dist2 = to_c.len2();
    let cos_max = (1.0 - LAMP_R * LAMP_R / dist2).max(0.0).sqrt();
    let w = to_c * (1.0 / dist2.sqrt());
    let (r1, r2) = (rnd(seed), rnd(seed));
    let cos_t = 1.0 - r1 * (1.0 - cos_max);
    let sin_t = (1.0 - cos_t * cos_t).sqrt();
    let phi = std::f32::consts::TAU * r2;
    let (a, b) = basis(w);
    let dir = (a * (phi.cos() * sin_t) + b * (phi.sin() * sin_t) + w * cos_t).norm();
    let cos_n = dir.dot(n);
    if cos_n <= 0.0 {
        return 0.0;
    }
    match intersect(scene, p + n * 1e-3, dir) {
        Some(h) if h.lamp => LAMP_POWER * cos_n * 2.0 * (1.0 - cos_max),
        _ => 0.0,
    }
}

fn trace(scene: &Scene, ray: (V3, V3), seed: &mut u32) -> f32 {
    let (mut o, mut d) = ray;
    let (mut radiance, mut throughput) = (0.0f32, 1.0f32);
    let mut specular_bounce = true; // câmera e reflexos especulares "veem" a lâmpada diretamente
    for depth in 0..MAX_DEPTH {
        let Some(h) = intersect(scene, o, d) else {
            radiance += throughput * sky(d);
            break;
        };
        if h.lamp {
            if specular_bounce {
                radiance += throughput * LAMP_POWER;
            }
            break;
        }
        let p = o + d * h.t;
        let (n, mat, albedo, rough) = if h.idx < 0 {
            // Chão: xadrez suave em tons de cinza
            let check = (((p.x * 0.9).floor() as i32 + (p.z * 0.9).floor() as i32) & 1) == 0;
            (v3(0.0, 1.0, 0.0), Mat::Diffuse, if check { 0.62 } else { 0.38 }, 0.0)
        } else {
            let sp = scene.spheres[h.idx as usize];
            ((p - sp.c) * (1.0 / sp.r), sp.m, sp.albedo, sp.rough)
        };
        match mat {
            Mat::Diffuse => {
                radiance += throughput * albedo * direct_light(scene, p, n, seed);
                throughput *= albedo;
                d = cosine_hemisphere(n, seed);
                o = p + n * 1e-3;
                specular_bounce = false;
            }
            Mat::Metal => {
                let r = d.reflect(n) + random_in_sphere(seed) * rough;
                if r.dot(n) <= 0.0 {
                    return radiance;
                }
                d = r.norm();
                o = p + n * 1e-3;
                throughput *= albedo;
                specular_bounce = true;
            }
            Mat::Glass => {
                const IOR: f32 = 1.5;
                let outside = d.dot(n) < 0.0;
                let nn = if outside { n } else { -n };
                let eta = if outside { 1.0 / IOR } else { IOR };
                let cos = (-d.dot(nn)).min(1.0);
                let sin2 = eta * eta * (1.0 - cos * cos);
                let mut r0 = (1.0 - IOR) / (1.0 + IOR);
                r0 *= r0;
                let fresnel = r0 + (1.0 - r0) * (1.0 - cos).powi(5);
                if sin2 > 1.0 || rnd(seed) < fresnel {
                    d = d.reflect(nn);
                    o = p + nn * 1e-3;
                } else {
                    d = (d * eta + nn * (eta * cos - (1.0 - sin2).sqrt())).norm();
                    o = p - nn * 1e-3;
                }
                throughput *= albedo;
                specular_bounce = true;
            }
        }
        // Roleta russa depois de alguns rebotes
        if depth >= 3 {
            let q = (1.0 - throughput).max(0.05);
            if rnd(seed) < q {
                break;
            }
            throughput /= 1.0 - q;
        }
    }
    radiance
}

/// Tone mapping (ACES aproximado) + gama 2.2 → cinza de 8 bits.
fn to_gray(v: f32) -> u8 {
    let v = v * 1.1;
    let v = v * (2.51 * v + 0.03) / (v * (2.43 * v + 0.59) + 0.14);
    (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8
}

// ---------------------------------------------------------------------------------------------
// Execução
// ---------------------------------------------------------------------------------------------

type Bucket = (usize, usize, usize, usize);

fn buckets(p: RenderParams) -> Vec<Bucket> {
    let mut list = Vec::new();
    for y in (0..p.height).step_by(BUCKET) {
        for x in (0..p.width).step_by(BUCKET) {
            list.push((x, y, BUCKET.min(p.width - x), BUCKET.min(p.height - y)));
        }
    }
    // Do centro para fora, como o Cinebench
    let (cx, cy) = (p.width as f64 / 2.0, p.height as f64 / 2.0);
    let key = |b: &Bucket| (b.0 as f64 + b.2 as f64 / 2.0 - cx).powi(2) + ((b.1 as f64 + b.3 as f64 / 2.0 - cy) * 1.6).powi(2);
    list.sort_by(|a, b| key(a).total_cmp(&key(b)));
    list
}

/// Estado compartilhado que a interface lê enquanto o teste roda.
pub struct Progress {
    pub params: RenderParams,
    pub pixels: Mutex<Vec<u8>>,
    pub buckets_done: AtomicUsize,
    pub buckets_total: AtomicUsize,
    pub samples: AtomicU64,
}

impl Progress {
    pub fn new(params: RenderParams) -> Arc<Progress> {
        Arc::new(Progress { params, pixels: Mutex::new(vec![0; params.width * params.height]), buckets_done: AtomicUsize::new(0), buckets_total: AtomicUsize::new(0), samples: AtomicU64::new(0) })
    }

    pub fn fraction(&self) -> f64 {
        let t = self.buckets_total.load(Relaxed);
        if t == 0 { 0.0 } else { self.buckets_done.load(Relaxed) as f64 / t as f64 }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.pixels.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

fn render_bucket(scene: &Scene, p: RenderParams, b: Bucket, cancel: &Cancel) -> Option<Vec<u8>> {
    let mut out = vec![0u8; b.2 * b.3];
    for y in b.1..b.1 + b.3 {
        if cancel.is_cancelled() {
            return None;
        }
        for x in b.0..b.0 + b.2 {
            let mut seed = ((y * p.width + x) as u32).wrapping_mul(9781).wrapping_add(0x9E37_79B9);
            let mut sum = 0.0f32;
            for _ in 0..p.spp {
                let u = (x as f32 + rnd(&mut seed)) / p.width as f32;
                let v = (y as f32 + rnd(&mut seed)) / p.height as f32;
                sum += trace(scene, scene.ray(u, v), &mut seed).min(4.0); // limita "fireflies"
            }
            out[(y - b.1) * b.2 + (x - b.0)] = to_gray(sum / p.spp as f32);
        }
    }
    Some(out)
}

/// Renderiza a imagem inteira com `threads` threads (multi-core e single-core fazem o mesmo trabalho).
/// Com `min_duration`, repete passadas até completar o tempo (desempenho sustentado).
pub fn run(threads: usize, min_duration: Duration, cancel: &Cancel, progress: &Arc<Progress>, on_pass: &(dyn Fn(u32) + Sync)) -> BenchResult {
    let p = progress.params;
    let threads = threads.max(1);
    let scene = Scene::new(p.width, p.height);
    let list = buckets(p);
    progress.buckets_total.store(list.len(), Relaxed);
    progress.samples.store(0, Relaxed);
    let start = Instant::now();
    let mut passes = 0u32;
    loop {
        progress.buckets_done.store(0, Relaxed);
        if let Ok(mut px) = progress.pixels.lock() {
            px.fill(0);
        }
        let next = AtomicUsize::new(0);
        thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Relaxed);
                        if i >= list.len() || cancel.is_cancelled() {
                            break;
                        }
                        let b = list[i];
                        let Some(data) = render_bucket(&scene, p, b, cancel) else { break };
                        if let Ok(mut px) = progress.pixels.lock() {
                            for row in 0..b.3 {
                                let dst = (b.1 + row) * p.width + b.0;
                                px[dst..dst + b.2].copy_from_slice(&data[row * b.2..(row + 1) * b.2]);
                            }
                        }
                        progress.samples.fetch_add((b.2 * b.3) as u64 * p.spp as u64, Relaxed);
                        progress.buckets_done.fetch_add(1, Relaxed);
                    }
                });
            }
        });
        if cancel.is_cancelled() {
            break;
        }
        passes += 1;
        on_pass(passes);
        if start.elapsed() >= min_duration {
            break;
        }
    }
    BenchResult { threads, seconds: start.elapsed().as_secs_f64(), samples: progress.samples.load(Relaxed), passes }
}

/// Prévia em texto (ASCII) da imagem, com `cols` colunas.
pub fn ascii_preview(pixels: &[u8], width: usize, height: usize, cols: usize) -> Vec<String> {
    const RAMP: &[u8] = b" .:-=+*#%@";
    if pixels.len() < width * height || width == 0 || height == 0 || cols == 0 {
        return Vec::new();
    }
    let cell_w = width as f64 / cols as f64;
    let cell_h = cell_w * 2.0; // caracteres são ~2x mais altos que largos
    let rows = ((height as f64 / cell_h).floor() as usize).max(1);
    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| {
                    let (x0, x1) = ((c as f64 * cell_w) as usize, (((c + 1) as f64 * cell_w) as usize).min(width).max((c as f64 * cell_w) as usize + 1));
                    let (y0, y1) = ((r as f64 * cell_h) as usize, (((r + 1) as f64 * cell_h) as usize).min(height).max((r as f64 * cell_h) as usize + 1));
                    let (mut sum, mut n) = (0u32, 0u32);
                    for y in y0..y1.min(height) {
                        for x in x0..x1.min(width) {
                            sum += pixels[y * width + x] as u32;
                            n += 1;
                        }
                    }
                    let g = if n == 0 { 0 } else { sum / n } as usize;
                    RAMP[g * (RAMP.len() - 1) / 255] as char
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: RenderParams = RenderParams { width: 96, height: 54, spp: 6 };

    fn render(threads: usize, params: RenderParams) -> (BenchResult, Vec<u8>) {
        let prog = Progress::new(params);
        let r = run(threads, Duration::ZERO, &Cancel::new(), &prog, &|_| {});
        (r, prog.snapshot())
    }

    #[test]
    fn image_is_identical_regardless_of_thread_count() {
        let (r1, img1) = render(1, SMALL);
        let (r2, img2) = render(3, SMALL);
        assert_eq!(img1, img2, "a imagem tem de ser determinística");
        assert_eq!(r1.samples, (SMALL.width * SMALL.height) as u64 * SMALL.spp as u64);
        assert_eq!(r1.samples, r2.samples);
        assert_eq!(r1.passes, 1);
        assert!(r1.score() > 0.0);
    }

    #[test]
    fn image_has_a_sensible_picture() {
        let (_, img) = render(2, SMALL);
        let mean = img.iter().map(|v| *v as f64).sum::<f64>() / img.len() as f64;
        assert!(mean > 15.0 && mean < 240.0, "média de brilho {mean}");
        let (min, max) = (*img.iter().min().unwrap(), *img.iter().max().unwrap());
        assert!(max - min > 80, "a imagem tem de ter contraste: {min}..{max}");
        // o céu (topo) é mais escuro que a lâmpada/objetos: linhas diferentes têm brilhos diferentes
        let row_mean = |y: usize| img[y * SMALL.width..(y + 1) * SMALL.width].iter().map(|v| *v as f64).sum::<f64>() / SMALL.width as f64;
        assert!((row_mean(2) - row_mean(45)).abs() > 3.0);
    }

    #[test]
    fn every_pixel_is_covered_by_a_bucket() {
        let p = RenderParams { width: 100, height: 70, spp: 1 };
        let list = buckets(p);
        let area: usize = list.iter().map(|b| b.2 * b.3).sum();
        assert_eq!(area, 100 * 70);
        // o primeiro bloco está perto do centro
        let first = list[0];
        assert!((first.0 as i64 + first.2 as i64 / 2 - 50).abs() <= 32);
    }

    #[test]
    fn multiple_passes_for_sustained_runs() {
        let prog = Progress::new(RenderParams { width: 32, height: 32, spp: 1 });
        let count = std::sync::atomic::AtomicU32::new(0);
        let r = run(1, Duration::from_millis(150), &Cancel::new(), &prog, &|_| {
            count.fetch_add(1, Relaxed);
        });
        assert!(r.passes >= 2, "passadas: {}", r.passes);
        assert_eq!(count.load(Relaxed), r.passes);
        assert!(r.seconds >= 0.15);
    }

    #[test]
    fn can_be_cancelled_mid_render() {
        let params = RenderParams { width: 480, height: 270, spp: 256 };
        let prog = Progress::new(params);
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            c2.cancel();
        });
        let t = Instant::now();
        let r = run(2, Duration::ZERO, &cancel, &prog, &|_| {});
        h.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(10), "cancelamento demorou {:?}", t.elapsed());
        assert_eq!(r.passes, 0);
        assert!(prog.fraction() < 1.0);
    }

    #[test]
    fn score_formula() {
        let r = BenchResult { threads: 1, seconds: 10.0, samples: 50_000_000, passes: 1 };
        assert!((r.score() - 500.0).abs() < 1e-9);
        assert_eq!(BenchResult { threads: 1, seconds: 0.0, samples: 5, passes: 0 }.score(), 0.0);
    }

    #[test]
    fn ascii_preview_maps_brightness() {
        let (w, h) = (40, 20);
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            for x in 20..w {
                px[y * w + x] = 255;
            }
        }
        let a = ascii_preview(&px, w, h, 10);
        assert!(!a.is_empty());
        assert!(a.iter().all(|l| l.chars().count() == 10));
        assert!(a[0].starts_with(' ') && a[0].ends_with('@'));
        assert!(ascii_preview(&[], 0, 0, 10).is_empty());
    }

    #[test]
    fn vector_math() {
        let a = v3(1.0, 0.0, 0.0);
        let b = v3(0.0, 1.0, 0.0);
        assert_eq!(a.cross(b), v3(0.0, 0.0, 1.0));
        assert_eq!(v3(1.0, -1.0, 0.0).norm().len2().round(), 1.0);
        let r = v3(1.0, -1.0, 0.0).reflect(b);
        assert_eq!(r, v3(1.0, 1.0, 0.0));
    }
}
