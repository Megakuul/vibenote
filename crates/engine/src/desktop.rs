//! Desktop integration (R-PLAT-004): the desktop entry and icons that let launchers and
//! compositors show vibenote with its icon. The window's app id is `vibenote`, so the entry is
//! `vibenote.desktop`. Packages install both system-wide; otherwise the app installs them for the
//! user — automatically as an AppImage, on request otherwise — never over a system-wide entry and
//! only over an entry it wrote itself.

use std::path::{Path, PathBuf};

/// The window's app id and the entry's file stem.
pub const APP_ID: &str = "vibenote";
/// Marks entries vibenote wrote (only those are ever rewritten).
const MARK: &str = "X-Vibenote-Managed=true";

static SVG: &[u8] = include_bytes!("../../../assets/icons/vibenote.svg");
static PNGS: [(u32, &[u8]); 8] = [
    (16, include_bytes!("../../../assets/icons/hicolor/16x16/vibenote.png")),
    (24, include_bytes!("../../../assets/icons/hicolor/24x24/vibenote.png")),
    (32, include_bytes!("../../../assets/icons/hicolor/32x32/vibenote.png")),
    (48, include_bytes!("../../../assets/icons/hicolor/48x48/vibenote.png")),
    (64, include_bytes!("../../../assets/icons/hicolor/64x64/vibenote.png")),
    (
        128,
        include_bytes!("../../../assets/icons/hicolor/128x128/vibenote.png"),
    ),
    (
        256,
        include_bytes!("../../../assets/icons/hicolor/256x256/vibenote.png"),
    ),
    (
        512,
        include_bytes!("../../../assets/icons/hicolor/512x512/vibenote.png"),
    ),
];

/// Where desktop files live (XDG base directories).
#[derive(Clone, Debug, PartialEq)]
pub struct Dirs {
    /// `$XDG_DATA_HOME` (default `~/.local/share`): the user's entries and icons.
    pub data_home: PathBuf,
    /// `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`): system-wide ones.
    pub data_dirs: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Option<Dirs> {
        let data_home = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
        };
        let dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        Some(Dirs {
            data_dirs: dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).collect(),
            data_home,
        })
    }
}

/// What [`install`] did.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "outcome", content = "path", rename_all = "snake_case")]
pub enum Outcome {
    Installed(PathBuf),
    Updated(PathBuf),
    /// Already there and current.
    Unchanged(PathBuf),
    /// A package installed vibenote system-wide: nothing to do.
    SystemEntry(PathBuf),
    /// The user has their own `vibenote.desktop`: left alone.
    Foreign(PathBuf),
}

impl Outcome {
    /// A sentence for the user.
    pub fn message(&self) -> String {
        match self {
            Outcome::Installed(p) => format!("vibenote was added to the app launcher ({})", p.display()),
            Outcome::Updated(p) => format!("vibenote's launcher entry was updated ({})", p.display()),
            Outcome::Unchanged(_) => "vibenote is already in the app launcher".into(),
            Outcome::SystemEntry(p) => format!("vibenote is installed system-wide ({})", p.display()),
            Outcome::Foreign(p) => format!("{} exists and was not written by vibenote; left as it is", p.display()),
        }
    }
}

/// The program a launcher should start: the AppImage file when running from one, the package's
/// wrapper (`$VIBENOTE_LAUNCHER`, set by the Nix package), else this executable.
pub fn launcher() -> Option<PathBuf> {
    for var in ["APPIMAGE", "VIBENOTE_LAUNCHER"] {
        if let Some(p) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(p));
        }
    }
    std::env::current_exe().ok()
}

/// `Exec=` argument quoting (Desktop Entry Specification).
fn exec_arg(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        return s.into_owned();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        // `%` is a field code: written as `%%`.
        if c == '%' {
            out.push('%');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The desktop entry starting `launcher`.
pub fn entry(launcher: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=vibenote\n\
         GenericName=Handwritten notes\n\
         Comment=A fast, paper-like handwriting workspace\n\
         Exec={} %U\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Office;Education;Graphics;\n\
         Keywords=notes;handwriting;pen;tablet;pdf;\n\
         StartupWMClass={APP_ID}\n\
         {MARK}\n",
        exec_arg(launcher)
    )
}

/// Writes `bytes` to `path` unless it already holds them (temp file, then rename).
fn put(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)?;
    Ok(true)
}

/// Installs the entry for `launcher` and the icons into `dirs.data_home` (R-PLAT-004).
pub fn install(dirs: &Dirs, launcher: &Path) -> Result<Outcome, String> {
    let name = format!("{APP_ID}.desktop");
    for d in &dirs.data_dirs {
        if *d == dirs.data_home {
            continue;
        }
        let p = d.join("applications").join(&name);
        if p.is_file() {
            return Ok(Outcome::SystemEntry(p));
        }
    }
    let path = dirs.data_home.join("applications").join(&name);
    let existed = match std::fs::read_to_string(&path) {
        Ok(old) if !old.lines().any(|l| l.trim() == MARK) => return Ok(Outcome::Foreign(path)),
        Ok(_) => true,
        Err(_) => false,
    };
    let icons = dirs.data_home.join("icons/hicolor");
    for (size, png) in &PNGS {
        put(&icons.join(format!("{size}x{size}/apps/{APP_ID}.png")), png)?;
    }
    put(&icons.join(format!("scalable/apps/{APP_ID}.svg")), SVG)?;
    let changed = put(&path, entry(launcher).as_bytes())?;
    Ok(match (existed, changed) {
        (false, _) => Outcome::Installed(path),
        (true, true) => Outcome::Updated(path),
        (true, false) => Outcome::Unchanged(path),
    })
}

/// At start: as an AppImage, installs (or updates, after the file moved) the user's entry;
/// otherwise nothing. Errors are only worth a log line.
pub fn auto_install() -> Option<Result<Outcome, String>> {
    let appimage = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty())?;
    let dirs = Dirs::from_env()?;
    Some(install(&dirs, Path::new(&appimage)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_is_quoted() {
        assert_eq!(
            exec_arg(Path::new("/opt/vibenote/bin/vibenote")),
            "/opt/vibenote/bin/vibenote"
        );
        assert_eq!(
            exec_arg(Path::new("/home/a b/My \"notes\" 100%.AppImage")),
            "\"/home/a b/My \\\"notes\\\" 100%%.AppImage\""
        );
    }
}
