//! Sandboxed PDF worker process (pdfium), ADR 0004.
//!
//! Started by `vibenote_pdf::PdfClient` with a Unix socket as stdin. It loads pdfium, then
//! sandboxes itself before reading any request: a seccomp filter blocks the network, running
//! programs, creating processes, writing files, ptrace and mount-class calls, and rlimits cap
//! memory and core dumps. Crashes and hangs only kill this process; the client restarts it.
//!
//! pdfium is found at `$VIBENOTE_PDFIUM` (file or directory), next to this executable, in
//! `$PDFIUM_DIR` (the nix dev shell sets it), or on the library path.

use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use vibenote_pdf::proto::{self, PageSize, Request, Response};

/// Most documents kept open at once (least recently used closed first).
const MAX_DOCS: usize = 8;
/// Address-space cap for the worker (bytes).
const MEMORY_CAP: u64 = 6 << 30;

fn library() -> Result<Box<dyn PdfiumLibraryBindings>, String> {
    let mut tried = Vec::new();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("VIBENOTE_PDFIUM") {
        let p = std::path::PathBuf::from(p);
        candidates.push(if p.is_dir() {
            Pdfium::pdfium_platform_library_name_at_path(&p)
        } else {
            p
        });
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    {
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(&dir));
    }
    if let Some(d) = std::env::var_os("PDFIUM_DIR") {
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(&std::path::PathBuf::from(
            d,
        )));
    }
    for c in candidates {
        if c.is_file() {
            match Pdfium::bind_to_library(&c) {
                Ok(b) => return Ok(b),
                Err(e) => tried.push(format!("{}: {e:?}", c.display())),
            }
        }
    }
    Pdfium::bind_to_system_library().map_err(|e| format!("libpdfium not found ({}; system: {e:?})", tried.join("; ")))
}

/// seccomp: these calls fail with EPERM; everything else pdfium needs keeps working.
fn sandbox() -> Result<(), String> {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};
    use std::collections::BTreeMap;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    let always = [
        // network
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        // programs and processes
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_fork,
        libc::SYS_vfork,
        libc::SYS_tkill,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        // file system changes
        libc::SYS_unlink,
        libc::SYS_unlinkat,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_mkdir,
        libc::SYS_mkdirat,
        libc::SYS_rmdir,
        libc::SYS_link,
        libc::SYS_linkat,
        libc::SYS_symlink,
        libc::SYS_symlinkat,
        libc::SYS_chmod,
        libc::SYS_fchmodat,
        libc::SYS_chown,
        libc::SYS_fchownat,
        libc::SYS_truncate,
        libc::SYS_creat,
        libc::SYS_mknod,
        libc::SYS_mknodat,
        // system
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_kexec_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_userfaultfd,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_open_by_handle_at,
        libc::SYS_io_uring_setup,
    ];
    for s in always {
        rules.insert(s, vec![]);
    }
    // Opening for writing or creating: open(path, flags) and openat(dir, path, flags).
    let write_flags = [libc::O_WRONLY, libc::O_RDWR, libc::O_CREAT, libc::O_TRUNC];
    let opens = |arg: u8| -> Result<Vec<SeccompRule>, String> {
        write_flags
            .iter()
            .map(|&f| {
                let f = u64::try_from(f).map_err(|e| e.to_string())?;
                let c = SeccompCondition::new(arg, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(f), f)
                    .map_err(|e| e.to_string())?;
                SeccompRule::new(vec![c]).map_err(|e| e.to_string())
            })
            .collect()
    };
    rules.insert(libc::SYS_open, opens(1)?);
    rules.insert(libc::SYS_openat, opens(2)?);
    // New processes: clone without CLONE_THREAD (threads keep working).
    let thread = u64::try_from(libc::CLONE_THREAD).map_err(|e| e.to_string())?;
    rules.insert(
        libc::SYS_clone,
        vec![
            SeccompRule::new(vec![
                SeccompCondition::new(0, SeccompCmpArgLen::Qword, SeccompCmpOp::MaskedEq(thread), 0)
                    .map_err(|e| e.to_string())?,
            ])
            .map_err(|e| e.to_string())?,
        ],
    );
    // Signals only to this process.
    let me = u64::from(std::process::id());
    let others = |arg: u8| -> Result<Vec<SeccompRule>, String> {
        Ok(vec![
            SeccompRule::new(vec![
                SeccompCondition::new(arg, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, me).map_err(|e| e.to_string())?,
            ])
            .map_err(|e| e.to_string())?,
        ])
    };
    rules.insert(libc::SYS_kill, others(0)?);
    rules.insert(libc::SYS_tgkill, others(0)?);
    apply(rules, libc::EPERM)?;
    // clone3 and openat2 take their flags in a struct the filter cannot read: ENOSYS, so glibc
    // falls back to clone/openat, which are filtered above. (Stacked filter: both apply.)
    let mut later: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    later.insert(libc::SYS_clone3, vec![]);
    later.insert(libc::SYS_openat2, vec![]);
    apply(later, libc::ENOSYS)
}

fn apply(rules: std::collections::BTreeMap<i64, Vec<seccompiler::SeccompRule>>, errno: i32) -> Result<(), String> {
    use seccompiler::{SeccompAction, SeccompFilter};
    let arch = std::env::consts::ARCH.try_into().map_err(|e| format!("{e:?}"))?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(u32::try_from(errno).unwrap_or(1)),
        arch,
    )
    .map_err(|e| e.to_string())?;
    let prog: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| e.to_string())?;
    seccompiler::apply_filter(&prog).map_err(|e| e.to_string())?;
    Ok(())
}

fn limits() -> Result<(), String> {
    use rustix::process::{Resource, Rlimit, setrlimit};
    let cap = |current: Option<u64>, want: u64| Some(current.map_or(want, |c| c.min(want)));
    let as_ = rustix::process::getrlimit(Resource::As);
    setrlimit(
        Resource::As,
        Rlimit {
            current: cap(as_.current, MEMORY_CAP),
            maximum: cap(as_.maximum, MEMORY_CAP),
        },
    )
    .map_err(|e| format!("rlimit: {e}"))?;
    setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )
    .map_err(|e| format!("rlimit: {e}"))?;
    Ok(())
}

/// What the sandbox blocks, tried for real (test hook).
fn probe() -> Response {
    let network_blocked =
        std::net::TcpStream::connect(("127.0.0.1", 9)).is_err_and(|e| e.raw_os_error() == Some(libc::EPERM));
    let exec_blocked = std::process::Command::new("/bin/true").status().is_err();
    let probe_file = std::env::temp_dir().join("vibenote-pdfd-probe");
    let write_blocked = std::fs::File::create(&probe_file).is_err();
    // Signal 0 to the parent: only checks permission, sends nothing.
    let kill_blocked = rustix::process::getppid()
        .is_some_and(|p| rustix::process::test_kill_process(p).is_err_and(|e| e == rustix::io::Errno::PERM));
    let openat2_blocked = rustix::fs::openat2(
        rustix::fs::CWD,
        &probe_file,
        rustix::fs::OFlags::CREATE | rustix::fs::OFlags::WRONLY,
        rustix::fs::Mode::from_raw_mode(0o600),
        rustix::fs::ResolveFlags::empty(),
    )
    .is_err_and(|e| e == rustix::io::Errno::NOSYS);
    Response::Probe {
        network_blocked,
        exec_blocked,
        write_blocked,
        kill_blocked,
        openat2_blocked,
    }
}

struct State<'a> {
    pdfium: &'a Pdfium,
    docs: HashMap<String, (u64, PdfDocument<'a>)>,
    tick: u64,
}

fn err(e: impl std::fmt::Debug) -> Response {
    Response::Error {
        message: format!("{e:?}"),
    }
}

impl<'a> State<'a> {
    fn doc(&mut self, key: &str) -> Option<&PdfDocument<'a>> {
        self.tick = self.tick.saturating_add(1);
        let t = self.tick;
        self.docs.get_mut(key).map(|(used, d)| {
            *used = t;
            &*d
        })
    }

    fn open(&mut self, key: String, bytes: Vec<u8>) -> Response {
        if self.docs.len() >= MAX_DOCS
            && let Some(old) = self.docs.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone())
        {
            self.docs.remove(&old);
        }
        let d = match self.pdfium.load_pdf_from_byte_vec(bytes, None) {
            Ok(d) => d,
            Err(e) => return err(e),
        };
        let pages = match d.pages().page_sizes() {
            Ok(s) => s
                .iter()
                .map(|r| PageSize {
                    width: f64::from(r.width().value),
                    height: f64::from(r.height().value),
                })
                .collect::<Vec<_>>(),
            Err(e) => return err(e),
        };
        if pages.is_empty() {
            return Response::Error {
                message: "the PDF has no pages".into(),
            };
        }
        self.tick = self.tick.saturating_add(1);
        self.docs.insert(key, (self.tick, d));
        Response::Opened { pages }
    }

    fn render(&mut self, key: &str, page: u32, width: u32) -> (Response, Option<std::os::fd::OwnedFd>) {
        let Some(d) = self.doc(key) else {
            return (err(proto::NOT_OPEN), None);
        };
        let Ok(index) = i32::try_from(page) else {
            return (err("page out of range"), None);
        };
        let p = match d.pages().get(index) {
            Ok(p) => p,
            Err(e) => return (err(e), None),
        };
        let w = i32::try_from(width.clamp(1, proto::MAX_SIDE)).unwrap_or(1);
        let side = i32::try_from(proto::MAX_SIDE).unwrap_or(i32::MAX);
        let cfg = PdfRenderConfig::new()
            .set_target_width(w)
            .set_maximum_height(side)
            .render_form_data(true);
        let bmp = match p.render_with_config(&cfg) {
            Ok(b) => b,
            Err(e) => return (err(e), None),
        };
        let (bw, bh) = (bmp.width(), bmp.height());
        let (Ok(bw), Ok(bh)) = (u32::try_from(bw), u32::try_from(bh)) else {
            return (err("bad bitmap size"), None);
        };
        if proto::bitmap_len(bw, bh).is_err() {
            return (err("page too large to render"), None);
        }
        // pdfium gives BGRA rows of `stride` bytes (opaque, on white paper): tightly packed RGBA.
        let raw = bmp.as_raw_bytes();
        let (w4, rows) = (
            usize::try_from(bw).unwrap_or(0).saturating_mul(4),
            usize::try_from(bh).unwrap_or(0),
        );
        let stride = raw.len().checked_div(rows).unwrap_or(0);
        if stride < w4 {
            return (err("bitmap smaller than announced"), None);
        }
        let mut px = Vec::with_capacity(w4.saturating_mul(rows));
        for row in raw.chunks_exact(stride).take(rows) {
            px.extend_from_slice(row.get(..w4).unwrap_or_default());
        }
        for c in px.as_chunks_mut::<4>().0 {
            c.swap(0, 2);
        }
        let fd = match rustix::fs::memfd_create("vibenote-page", rustix::fs::MemfdFlags::CLOEXEC) {
            Ok(fd) => fd,
            Err(e) => return (err(e), None),
        };
        let mut f = std::fs::File::from(fd);
        if let Err(e) = std::io::Write::write_all(&mut f, &px) {
            return (err(e), None);
        }
        (
            Response::Rendered { width: bw, height: bh },
            Some(std::os::fd::OwnedFd::from(f)),
        )
    }

    fn text(&mut self, key: &str, page: u32) -> Response {
        let Some(d) = self.doc(key) else {
            return err(proto::NOT_OPEN);
        };
        let Ok(index) = i32::try_from(page) else {
            return err("page out of range");
        };
        match d.pages().get(index).and_then(|p| p.text().map(|t| t.all())) {
            Ok(text) => Response::Text { text },
            Err(e) => err(e),
        }
    }
}

fn serve(sock: &UnixStream, pdfium: &Pdfium) -> Result<(), proto::ProtoError> {
    let test = std::env::var_os("VIBENOTE_PDFD_TEST").is_some_and(|v| v == "1");
    let mut st = State {
        pdfium,
        docs: HashMap::new(),
        tick: 0,
    };
    loop {
        let (req, _) = match proto::recv::<Request>(sock) {
            Ok(r) => r,
            // The app closed the socket: done.
            Err(proto::ProtoError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let (resp, fd) = match req {
            Request::Open { key, payload } => {
                let bytes = proto::recv_payload(sock, payload)?;
                (st.open(key, bytes), None)
            }
            Request::Render { key, page, width } => st.render(&key, page, width),
            Request::Text { key, page } => (st.text(&key, page), None),
            Request::Close { key } => {
                st.docs.remove(&key);
                (Response::Closed, None)
            }
            Request::Probe if test => (probe(), None),
            Request::Probe => (err("probe disabled"), None),
        };
        proto::send(sock, &resp, None, fd.as_ref())?;
    }
}

fn main() -> std::process::ExitCode {
    let sock = match std::io::stdin().as_fd().try_clone_to_owned() {
        Ok(fd) => UnixStream::from(fd),
        Err(e) => {
            eprintln!("vibenote-pdfd: no socket on stdin: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let pdfium = match library() {
        Ok(b) => Pdfium::new(b),
        Err(e) => {
            eprintln!("vibenote-pdfd: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    // Sandbox before touching any input.
    if let Err(e) = limits().and_then(|()| sandbox()) {
        eprintln!("vibenote-pdfd: sandbox: {e}");
        return std::process::ExitCode::FAILURE;
    }
    match serve(&sock, &pdfium) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vibenote-pdfd: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
