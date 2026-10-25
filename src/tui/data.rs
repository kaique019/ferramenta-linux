//! Dados da interface: o que é lido uma vez (`Static`) e o que é atualizado a cada poucos segundos (`Live`).
//! A coleta roda em threads; a interface só recebe mensagens e nunca espera por /sys, nvidia-smi ou dmidecode.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::settings::SettingsStore;
use crate::sensors::{CpuLoad, CpuSampler, SensorReading};
use crate::util::Roots;
use crate::{autostart, gpu, network, power, sensors, serial, storage, sysinfo};

/// Tudo de que a interface precisa para falar com o sistema; nos testes aponta para pastas temporárias.
#[derive(Clone)]
pub struct Env {
    pub roots: Roots,
    pub store: SettingsStore,
    pub autostart: autostart::Dirs,
    pub home: PathBuf,
}

impl Env {
    pub fn system() -> Env {
        Env { roots: Roots::system(), store: SettingsStore::default_location(), autostart: autostart::Dirs::system_default(), home: crate::util::home_dir() }
    }
}

#[derive(Debug, Clone)]
pub struct Static {
    pub os: sysinfo::OsInfo,
    pub board: sysinfo::BoardInfo,
    pub cpu: sysinfo::CpuInfo,
    pub disks: Vec<storage::Disk>,
    pub displays: Vec<sysinfo::DisplayInfo>,
    pub interfaces: Vec<network::Interface>,
    pub dns: Vec<String>,
    pub ports: Vec<serial::SerialPort>,
    pub serial_advice: Vec<serial::Advice>,
}

#[derive(Debug, Clone)]
pub struct Live {
    pub cpu: CpuLoad,
    pub mem: sysinfo::MemInfo,
    pub sensors: Vec<SensorReading>,
    pub gpus: Vec<gpu::GpuInfo>,
    pub power: power::PowerInfo,
    pub mounts: Vec<storage::Mount>,
}

pub enum Msg {
    Static(Box<Static>),
    Live(Box<Live>),
}

pub fn collect_static(env: &Env) -> Static {
    let r = &env.roots;
    let ports = serial::list(r);
    let serial_advice = serial::diagnose(&ports, &serial::gather_facts());
    Static { os: sysinfo::collect_os(r), board: sysinfo::collect_board(r), cpu: sysinfo::collect_cpu(r), disks: storage::collect_disks(r, false), displays: sysinfo::collect_displays(r), interfaces: network::interfaces(r), dns: network::dns_servers(r), ports, serial_advice }
}

pub fn collect_live(env: &Env, sampler: &mut CpuSampler) -> Live {
    let r = &env.roots;
    Live { cpu: sampler.sample(), mem: sysinfo::collect_mem(r, false), sensors: sensors::read_all(r), gpus: gpu::collect(r), power: power::collect(r), mounts: storage::collect_mounts(r) }
}

/// Inicia as threads de coleta. Elas terminam sozinhas quando o receptor é descartado.
pub fn spawn_collectors(env: Env, every: Duration) -> Receiver<Msg> {
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let (e1, t1) = (env.clone(), tx.clone());
    thread::spawn(move || {
        let _ = t1.send(Msg::Static(Box::new(collect_static(&e1))));
    });
    thread::spawn(move || {
        let mut sampler = CpuSampler::new(&env.roots);
        loop {
            let live = collect_live(&env, &mut sampler);
            if tx.send(Msg::Live(Box::new(live))).is_err() {
                break;
            }
            thread::sleep(every);
        }
    });
    rx
}
