//! Configurações e histórico persistidos em `~/.config/resetexplode/settings.json`.
//!
//! Diferenças em relação ao projeto Windows (problemas encontrados na análise):
//! * a gravação é atômica (arquivo temporário + rename) e o arquivo anterior vira `settings.json.bak`;
//! * se o JSON estiver corrompido ele é renomeado para `settings.json.corrupt-<hora>` (não é apagado) e o
//!   programa avisa, em vez de zerar em silêncio os backups que servem para "restaurar originais";
//! * o arquivo tem permissão 0600, porque pode guardar uma chave de API.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::diag;
use crate::util::{app_config_dir, as_user, now_unix, write_atomic_mode};

/// Quantos resultados de benchmark ficam no histórico.
pub const HISTORY_LEN: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchRun {
    pub date: u64,
    pub multi: Option<f64>,
    pub single: Option<f64>,
    pub sustained: bool,
    pub cpu: String,
}

/// Resultado dos demais benchmarks (`test` = "sim", "mem", "load", "disk").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchRecord {
    pub date: u64,
    pub test: String,
    pub score: f64,
    pub summary: String,
}

/// Programa auxiliar que abre antes do jogo e pode fechar junto com ele.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HelperProgram {
    pub path: String,
    pub args: String,
    pub close_with_game: bool,
}

impl Default for HelperProgram {
    fn default() -> Self {
        HelperProgram { path: String::new(), args: String::new(), close_with_game: true }
    }
}

/// Perfil de um jogo: desempenho, argumentos de gráficos e programas auxiliares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    /// "normal" | "above" | "high" (valores negativos de nice exigem privilégios).
    pub priority: String,
    pub gamemode: bool,
    pub mangohud: bool,
    /// Usar a GPU dedicada (PRIME render offload).
    pub dgpu: bool,
    /// Governador de CPU "performance" enquanto o jogo roda (volta ao anterior ao fechar).
    pub performance_governor: bool,
    pub exe_path: Option<String>,
    /// "default" | "full" | "borderless" | "window"
    pub win_mode: String,
    /// "default" | "1280x720" | "1920x1080" | "2560x1440" | "3840x2160"
    pub res: String,
    /// 0 = não definir.
    pub hz: u32,
    /// "source" | "unity" | "unreal"
    pub style: String,
    pub extra: String,
    /// Variáveis de ambiente extras, uma por linha: `CHAVE=valor`.
    pub env: String,
    pub helpers: Vec<HelperProgram>,
}

impl Default for GameProfile {
    fn default() -> Self {
        GameProfile {
            priority: "normal".into(),
            gamemode: false,
            mangohud: false,
            dgpu: false,
            performance_governor: false,
            exe_path: None,
            win_mode: "default".into(),
            res: "default".into(),
            hz: 0,
            style: "source".into(),
            extra: String::new(),
            env: String::new(),
            helpers: Vec::new(),
        }
    }
}

/// Jogo adicionado à mão (qualquer executável).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomGame {
    pub name: String,
    pub exe: String,
    pub args: String,
}

/// Configuração do overlay (gera o arquivo do MangoHud).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Chaves de `overlay::METRICS`; `None` = padrão.
    pub items: Option<Vec<String>>,
    /// "TL" | "TR" | "BL" | "BR"
    pub corner: String,
    pub scale: f64,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        OverlayConfig { items: None, corner: "TL".into(), scale: 1.0 }
    }
}

/// Programa de inicialização removido, para "Restaurar removidos".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StartupBackup {
    pub name: String,
    /// Caminho original do arquivo `.desktop`.
    pub original: String,
    /// Cópia guardada na pasta de estado.
    pub backup: String,
    pub removed_at: u64,
}

/// Resultado do verificador de estabilidade (RESET OC).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OcRecord {
    pub date: u64,
    /// "cpu" | "gpu" | "ambos"
    pub target: String,
    pub minutes: f64,
    pub verdict: String,
    pub summary: String,
    pub max_cpu_temp: Option<f64>,
    pub max_gpu_temp: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub bench_runs: Vec<BenchRun>,
    pub bench_records: Vec<BenchRecord>,
    /// Valor original de cada ajuste antes da primeira alteração (`None` = não existia).
    pub tweak_backup: BTreeMap<String, Option<String>>,
    pub startup_removed: Vec<StartupBackup>,
    pub game_profiles: BTreeMap<String, GameProfile>,
    pub custom_games: Vec<CustomGame>,
    pub overlay: OverlayConfig,
    pub oc_history: Vec<OcRecord>,
    /// Limite de energia original de cada GPU NVIDIA (watts), por UUID, para restaurar.
    pub gpu_power_backup: BTreeMap<String, f64>,
    pub light_theme: bool,
    /// Chave da API da HowManyFPS (opcional).
    pub howmanyfps_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: 1,
            bench_runs: Vec::new(),
            bench_records: Vec::new(),
            tweak_backup: BTreeMap::new(),
            startup_removed: Vec::new(),
            game_profiles: BTreeMap::new(),
            custom_games: Vec::new(),
            overlay: OverlayConfig::default(),
            oc_history: Vec::new(),
            gpu_power_backup: BTreeMap::new(),
            light_theme: false,
            howmanyfps_key: None,
        }
    }
}

impl Settings {
    /// Guarda o resultado do benchmark de CPU (o mais recente primeiro).
    pub fn push_bench_run(&mut self, run: BenchRun) {
        self.bench_runs.insert(0, run);
        self.bench_runs.truncate(HISTORY_LEN);
    }

    /// Guarda o resultado de outro benchmark, mantendo `HISTORY_LEN` por tipo de teste.
    pub fn push_bench_record(&mut self, rec: BenchRecord) {
        let test = rec.test.clone();
        self.bench_records.insert(0, rec);
        let mut seen = 0;
        self.bench_records.retain(|r| {
            if r.test != test {
                return true;
            }
            seen += 1;
            seen <= HISTORY_LEN
        });
    }

    pub fn push_oc_record(&mut self, rec: OcRecord) {
        self.oc_history.insert(0, rec);
        self.oc_history.truncate(HISTORY_LEN);
    }

    /// Resultados de um tipo de teste, do mais recente para o mais antigo.
    pub fn records_of(&self, test: &str) -> Vec<&BenchRecord> {
        self.bench_records.iter().filter(|r| r.test == test).collect()
    }
}

/// Resultado de carregar o arquivo de configurações.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// Aviso para mostrar ao usuário (arquivo corrompido ou ilegível).
    pub warning: Option<String>,
}

static LOCK: Mutex<()> = Mutex::new(());

/// Tamanho máximo aceito para o arquivo de configurações (o normal tem alguns KiB).
const MAX_SETTINGS_BYTES: u64 = 8 << 20;

/// Lê só arquivo comum e de tamanho razoável (um symlink para /dev/zero não esgota a memória).
fn read_limited(path: &Path, max: u64) -> io::Result<String> {
    use std::io::Read;
    let f = fs::File::open(path)?;
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "não é um arquivo comum"));
    }
    if md.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("arquivo grande demais ({} bytes)", md.len())));
    }
    let mut s = String::new();
    f.take(max).read_to_string(&mut s)?;
    Ok(s)
}

/// Acesso ao arquivo de configurações.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SettingsStore { path: path.into() }
    }

    /// `~/.config/resetexplode/settings.json` (do usuário que chamou o sudo, se for o caso).
    pub fn default_location() -> Self {
        Self::new(app_config_dir().join("settings.json"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn suffixed(&self, suffix: &str) -> PathBuf {
        let mut s = self.path.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    }

    pub fn load(&self) -> Loaded {
        // roda com a identidade do usuário-alvo: um symlink no lugar do arquivo não faz o root ler o que não deveria
        match as_user(|| self.load_inner()) {
            Ok(l) => l,
            Err(e) => Loaded { settings: Settings::default(), warning: Some(e) },
        }
    }

    fn load_inner(&self) -> Loaded {
        let text = match read_limited(&self.path, MAX_SETTINGS_BYTES) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded { settings: Settings::default(), warning: None },
            Err(e) => {
                let msg = format!("Não consegui ler {}: {e}. Usando configurações padrão (nada será gravado por cima até você salvar algo).", self.path.display());
                diag::warn("settings", &msg);
                return Loaded { settings: Settings::default(), warning: Some(msg) };
            }
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(s) => Loaded { settings: s, warning: None },
            Err(e) => {
                let kept = self.suffixed(&format!(".corrupt-{}", now_unix()));
                let moved = fs::rename(&self.path, &kept).is_ok();
                let msg = if moved {
                    format!("O arquivo de configurações estava corrompido ({e}). Guardei uma cópia em {} e comecei com as configurações padrão.", kept.display())
                } else {
                    format!("O arquivo de configurações está corrompido ({e}) e não consegui guardar uma cópia. Usando configurações padrão.")
                };
                diag::warn("settings", &msg);
                Loaded { settings: Settings::default(), warning: Some(msg) }
            }
        }
    }

    pub fn save(&self, s: &Settings) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
        if self.path.exists()
            && let Err(e) = fs::copy(&self.path, self.suffixed(".bak")) {
                diag::warn("settings", format!("Não consegui criar a cópia .bak das configurações: {e}"));
            }
        write_atomic_mode(&self.path, &json, 0o600)
    }

    /// Carrega, altera e grava (sob um trava, para duas tarefas não perderem as alterações uma da outra).
    pub fn update<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> io::Result<R> {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = self.load().settings;
        let r = f(&mut s);
        self.save(&s)?;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let d = tempfile::tempdir().unwrap();
        let s = SettingsStore::new(d.path().join("cfg/settings.json"));
        (d, s)
    }

    #[test]
    fn missing_file_gives_defaults_without_warning() {
        let (_d, s) = store();
        let l = s.load();
        assert_eq!(l.settings, Settings::default());
        assert!(l.warning.is_none());
    }

    #[test]
    fn roundtrip_and_permissions() {
        let (_d, s) = store();
        s.update(|st| {
            st.light_theme = true;
            st.tweak_backup.insert("governor".into(), Some("powersave".into()));
            st.tweak_backup.insert("swappiness".into(), None);
            st.game_profiles.insert("Steam:730".into(), GameProfile { gamemode: true, hz: 144, ..Default::default() });
        })
        .unwrap();
        let l = s.load();
        assert!(l.warning.is_none());
        assert!(l.settings.light_theme);
        assert_eq!(l.settings.tweak_backup["governor"], Some("powersave".to_string()));
        assert_eq!(l.settings.tweak_backup["swappiness"], None);
        assert_eq!(l.settings.game_profiles["Steam:730"].hz, 144);
        let mode = fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "o arquivo de configurações deve ser privado");
    }

    #[test]
    fn corrupt_file_is_preserved_not_erased() {
        let (_d, s) = store();
        s.update(|st| st.light_theme = true).unwrap();
        fs::write(s.path(), "{ isto não é json").unwrap();
        let l = s.load();
        assert!(l.warning.as_deref().unwrap().contains("corrompido"));
        assert_eq!(l.settings, Settings::default());
        let dir = s.path().parent().unwrap();
        let kept: Vec<_> = fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).collect();
        assert_eq!(kept.len(), 1, "a cópia do arquivo corrompido tem de existir");
        assert_eq!(fs::read_to_string(kept[0].path()).unwrap(), "{ isto não é json");
    }

    #[test]
    fn save_keeps_previous_version_as_bak() {
        let (_d, s) = store();
        s.update(|st| st.light_theme = false).unwrap();
        s.update(|st| st.light_theme = true).unwrap();
        let bak = fs::read_to_string(s.suffixed(".bak")).unwrap();
        let old: Settings = serde_json::from_str(&bak).unwrap();
        assert!(!old.light_theme);
        assert!(s.load().settings.light_theme);
    }

    #[test]
    fn old_files_with_missing_fields_still_load() {
        let (_d, s) = store();
        fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        fs::write(s.path(), r#"{"light_theme": true, "campo_do_futuro": 1}"#).unwrap();
        let l = s.load();
        assert!(l.warning.is_none());
        assert!(l.settings.light_theme);
        assert!(l.settings.bench_runs.is_empty());
    }

    #[test]
    fn history_is_trimmed() {
        let mut st = Settings::default();
        for i in 0..15 {
            st.push_bench_run(BenchRun { date: i, multi: Some(i as f64), single: None, sustained: false, cpu: "x".into() });
            st.push_bench_record(BenchRecord { date: i, test: "sim".into(), score: i as f64, summary: String::new() });
            st.push_bench_record(BenchRecord { date: i, test: "mem".into(), score: i as f64, summary: String::new() });
        }
        assert_eq!(st.bench_runs.len(), HISTORY_LEN);
        assert_eq!(st.bench_runs[0].date, 14, "o mais recente vem primeiro");
        assert_eq!(st.records_of("sim").len(), HISTORY_LEN);
        assert_eq!(st.records_of("mem").len(), HISTORY_LEN);
        assert_eq!(st.records_of("sim")[0].date, 14);
    }

    #[test]
    fn helper_programs_close_with_game_by_default() {
        let h: HelperProgram = serde_json::from_str(r#"{"path":"/usr/bin/x"}"#).unwrap();
        assert!(h.close_with_game);
    }
}
