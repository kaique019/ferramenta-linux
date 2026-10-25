//! Overlay de desempenho: gera o arquivo de configuração do MangoHud a partir das escolhas do usuário.

use std::path::{Path, PathBuf};

use crate::settings::OverlayConfig;
use crate::util::{config_dir, create_dir_for_user, give_to_user, write_atomic};

/// (chave, opção do MangoHud, rótulo)
pub const METRICS: &[(&str, &str, &str)] = &[
    ("fps", "fps", "FPS"),
    ("frametime", "frametime", "Tempo de frame"),
    ("frame_graph", "frame_timing", "Gráfico de frametime"),
    ("cpu_load", "cpu_stats", "Uso da CPU"),
    ("cpu_temp", "cpu_temp", "Temperatura da CPU"),
    ("cpu_power", "cpu_power", "Consumo da CPU"),
    ("gpu_load", "gpu_stats", "Uso da GPU"),
    ("gpu_temp", "gpu_temp", "Temperatura da GPU"),
    ("gpu_power", "gpu_power", "Consumo da GPU"),
    ("gpu_clock", "gpu_core_clock", "Clock da GPU"),
    ("vram", "vram", "VRAM"),
    ("ram", "ram", "RAM"),
];

pub const DEFAULT_ITEMS: &[&str] = &["fps", "frametime", "cpu_load", "cpu_temp", "gpu_load", "gpu_temp", "vram", "ram"];

const MARKER: &str = "# gerado por resetexplode";

fn position(corner: &str) -> &'static str {
    match corner {
        "TR" => "top-right",
        "BL" => "bottom-left",
        "BR" => "bottom-right",
        _ => "top-left",
    }
}

/// Texto do `MangoHud.conf`. Chaves desconhecidas são ignoradas, e a escala é limitada a 0,5–3.
pub fn generate(cfg: &OverlayConfig) -> String {
    let items: Vec<String> = match &cfg.items {
        Some(v) => v.clone(),
        None => DEFAULT_ITEMS.iter().map(|s| s.to_string()).collect(),
    };
    let scale = if cfg.scale.is_finite() { cfg.scale.clamp(0.5, 3.0) } else { 1.0 };
    let mut out = format!("{MARKER}\nposition={}\nfont_size={}\nbackground_alpha=0.4\n", position(&cfg.corner), (24.0 * scale).round() as u32);
    for (key, opt, _) in METRICS {
        if items.iter().any(|i| i == key) {
            out.push_str(opt);
            out.push('\n');
        }
    }
    out
}

pub fn default_path() -> PathBuf {
    config_dir().join("MangoHud/MangoHud.conf")
}

/// Grava a configuração. Se já existir um arquivo que não é nosso, guarda uma cópia `.bak` (uma vez) antes.
pub fn write(cfg: &OverlayConfig, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        create_dir_for_user(dir).map_err(|e| format!("não consegui criar {}: {e}", dir.display()))?;
    }
    if let Ok(old) = std::fs::read_to_string(path) {
        let bak = path.with_extension("conf.bak");
        if !old.starts_with(MARKER) && !bak.exists() {
            std::fs::write(&bak, old).map_err(|e| format!("falha ao guardar o arquivo antigo: {e}"))?;
            give_to_user(&bak);
        }
    }
    write_atomic(path, generate(cfg).as_bytes()).map_err(|e| format!("não consegui gravar {}: {e}", path.display()))?;
    give_to_user(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_default_metrics_and_position() {
        let t = generate(&OverlayConfig::default());
        assert!(t.starts_with(MARKER));
        assert!(t.contains("position=top-left\n") && t.contains("font_size=24\n"));
        for m in ["fps", "cpu_stats", "gpu_temp", "vram"] {
            assert!(t.lines().any(|l| l == m), "{m}");
        }
        assert!(!t.lines().any(|l| l == "cpu_power"));
    }

    #[test]
    fn custom_items_corner_and_scale_are_applied_and_clamped() {
        let cfg = OverlayConfig { items: Some(vec!["fps".into(), "cpu_power".into(), "nao_existe".into()]), corner: "BR".into(), scale: 10.0 };
        let t = generate(&cfg);
        assert!(t.contains("position=bottom-right") && t.contains("font_size=72"));
        assert!(t.lines().any(|l| l == "cpu_power") && !t.contains("nao_existe") && !t.lines().any(|l| l == "ram"));
        assert!(generate(&OverlayConfig { scale: f64::NAN, ..Default::default() }).contains("font_size=24"));
        assert!(generate(&OverlayConfig { corner: "??".into(), ..Default::default() }).contains("top-left"));
    }

    #[test]
    fn write_backs_up_foreign_file_once_and_overwrites_our_own() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("MangoHud/MangoHud.conf");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "meu_config=1\n").unwrap();
        write(&OverlayConfig::default(), &p).unwrap();
        assert_eq!(std::fs::read_to_string(p.with_extension("conf.bak")).unwrap(), "meu_config=1\n");
        write(&OverlayConfig { corner: "TR".into(), ..Default::default() }, &p).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("top-right"));
        assert_eq!(std::fs::read_to_string(p.with_extension("conf.bak")).unwrap(), "meu_config=1\n", "o backup original não é trocado");
    }
}
