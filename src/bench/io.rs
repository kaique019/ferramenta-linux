//! E/S de benchmark: buffers alinhados e abertura de arquivos com `O_DIRECT`, para medir o disco e não o cache
//! de páginas do kernel (o equivalente do `FILE_FLAG_NO_BUFFERING` do Windows).
//!
//! Alguns sistemas de arquivos (tmpfs, ecryptfs, certos FUSE) não aceitam `O_DIRECT`. Nesses casos o arquivo é aberto
//! normalmente e o cache é descartado com `posix_fadvise(DONTNEED)`; o resultado informa `direct = false`.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Alinhamento aceito por qualquer dispositivo (setor lógico de até 4 KiB).
pub const ALIGN: usize = 4096;

/// Buffer de bytes alinhado em 4 KiB, exigido pelo `O_DIRECT`.
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

// SAFETY: o buffer é o único dono da memória apontada por `ptr`; mover para outra thread é seguro.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    /// Aloca `len` bytes (arredondado para cima até múltiplo de 4 KiB), zerados.
    pub fn new(len: usize) -> AlignedBuf {
        let len = len.max(1).next_multiple_of(ALIGN);
        let layout = Layout::from_size_align(len, ALIGN).expect("layout válido");
        // SAFETY: `layout` tem tamanho diferente de zero.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        AlignedBuf { ptr, len, layout }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` aponta para `len` bytes inicializados (alloc_zeroed) e vivos enquanto `self` existir.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: como acima, e `&mut self` garante acesso exclusivo.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: `ptr` foi alocado com exatamente este `layout`.
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

/// Gerador xorshift64*: rápido e determinístico (as sementes fixas tornam a carga idêntica em toda execução).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Número em `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

/// Preenche com dados pseudoaleatórios (incompressíveis: não deixam o controlador do SSD "trapacear").
pub fn fill_random(buf: &mut [u8], seed: u64) {
    let mut r = Rng::new(seed);
    for chunk in buf.chunks_mut(8) {
        let v = r.next_u64().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Abre um arquivo existente só para leitura.
    ReadOnly,
    /// Cria (ou zera) o arquivo para leitura e escrita, com escrita síncrona (`O_DSYNC`).
    CreateWrite,
}

/// Arquivo aberto para o benchmark.
pub struct Bench {
    pub file: File,
    /// `true` se o `O_DIRECT` está valendo; `false` se foi preciso usar o cache e descartá-lo.
    pub direct: bool,
}

impl Bench {
    /// Descarta do cache de páginas o que foi lido ou escrito (só tem efeito sem `O_DIRECT`).
    pub fn drop_cache(&self) {
        if !self.direct {
            // SAFETY: o descritor é válido enquanto `self.file` existir; os argumentos são constantes simples.
            unsafe {
                libc::posix_fadvise(self.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
            }
        }
    }
}

fn open_with(path: &Path, access: Access, direct: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    let mut flags = 0;
    match access {
        Access::ReadOnly => {
            o.read(true);
        }
        Access::CreateWrite => {
            o.read(true).write(true).create(true).truncate(true);
            flags |= libc::O_DSYNC;
        }
    }
    if direct {
        flags |= libc::O_DIRECT;
    }
    o.custom_flags(flags).open(path)
}

/// Abre `path` para o benchmark, com E/S direta quando o sistema de arquivos permitir.
pub fn open_bench(path: &Path, access: Access) -> io::Result<Bench> {
    if let Ok(f) = open_with(path, access, true) {
        // Alguns sistemas aceitam o open mas recusam o primeiro acesso com EINVAL: testa antes de confiar.
        let mut probe = AlignedBuf::new(ALIGN);
        let ok = match access {
            Access::ReadOnly => !matches!(f.read_at(probe.as_mut_slice(), 0), Err(e) if e.raw_os_error() == Some(libc::EINVAL)),
            Access::CreateWrite => !matches!(f.write_at(probe.as_slice(), 0), Err(e) if e.raw_os_error() == Some(libc::EINVAL)),
        };
        if ok {
            return Ok(Bench { file: f, direct: true });
        }
    }
    let f = open_with(path, access, false)?;
    let b = Bench { file: f, direct: false };
    b.drop_cache();
    Ok(b)
}

/// Pasta temporária de teste: é apagada (com tudo dentro) ao sair de escopo, inclusive em caso de erro ou cancelamento.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Cria `<parent>/.resetexplode-bench-<pid>-<n>`. Falha se já existir, para nunca apagar dados de outra pessoa.
    pub fn create_in(parent: &Path) -> io::Result<TempDir> {
        use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
        static N: AtomicU32 = AtomicU32::new(0);
        let path = parent.join(format!(".resetexplode-bench-{}-{}", std::process::id(), N.fetch_add(1, Relaxed)));
        std::fs::create_dir(&path)?;
        Ok(TempDir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buffer_is_aligned_and_rounded() {
        let mut b = AlignedBuf::new(5000);
        assert_eq!(b.as_slice().as_ptr() as usize % ALIGN, 0);
        assert_eq!(b.len(), 8192);
        assert!(b.as_slice().iter().all(|v| *v == 0));
        b.as_mut_slice()[8191] = 7;
        assert_eq!(b.as_slice()[8191], 7);
        assert_eq!(AlignedBuf::new(0).len(), ALIGN);
    }

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        assert!((0..100).all(|_| a.next_u64() == b.next_u64()));
        let mut c = Rng::new(43);
        assert_ne!(Rng::new(42).next_u64(), c.next_u64());
        let mut r = Rng::new(1);
        assert!((0..1000).all(|_| r.below(7) < 7));
        assert_eq!(r.below(0), 0, "n = 0 não pode dividir por zero");
    }

    #[test]
    fn random_fill_is_reproducible_and_not_constant() {
        let (mut a, mut b) = (vec![0u8; 1003], vec![0u8; 1003]);
        fill_random(&mut a, 9);
        fill_random(&mut b, 9);
        assert_eq!(a, b);
        assert!(a.iter().any(|v| *v != a[0]));
        let mut c = vec![0u8; 1003];
        fill_random(&mut c, 10);
        assert_ne!(a, c);
    }

    #[test]
    fn temp_dir_is_removed_on_drop_and_never_reused() {
        let base = tempfile::tempdir().unwrap();
        let (p1, p2);
        {
            let a = TempDir::create_in(base.path()).unwrap();
            let b = TempDir::create_in(base.path()).unwrap();
            assert_ne!(a.path(), b.path());
            std::fs::write(a.path().join("x"), b"1").unwrap();
            p1 = a.path().to_path_buf();
            p2 = b.path().to_path_buf();
            assert!(p1.exists() && p2.exists());
        }
        assert!(!p1.exists() && !p2.exists());
    }

    #[test]
    fn direct_or_fallback_io_roundtrip() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("f.bin");
        let mut data = AlignedBuf::new(8192);
        fill_random(data.as_mut_slice(), 5);
        let w = open_bench(&path, Access::CreateWrite).unwrap();
        w.file.write_all_at(data.as_slice(), 0).unwrap();
        drop(w);
        let r = open_bench(&path, Access::ReadOnly).unwrap();
        let mut back = AlignedBuf::new(8192);
        r.file.read_exact_at(back.as_mut_slice(), 0).unwrap();
        assert_eq!(back.as_slice(), data.as_slice());
        // com ou sem O_DIRECT (depende do sistema de arquivos de /tmp) o resultado é o mesmo
        let _ = r.direct;
        assert!(open_bench(&base.path().join("nao-existe"), Access::ReadOnly).is_err());
    }
}
