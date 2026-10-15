//! Language packs (R-HWR-007, ADR 0006 decision 1): a directory with `manifest.json`, an ONNX
//! recogniser and its character list. Packs are found in directories (the bundled one, user
//! ones); manifests are hostile input and are checked.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Largest manifest, model and character list read (bytes).
const MAX_MANIFEST: u64 = 64 << 10;
pub const MAX_MODEL: u64 = 512 << 20;
const MAX_DICT: u64 = 16 << 20;
const MAX_LEXICON: u64 = 64 << 20;
/// Most lexicon words (hostile packs).
const MAX_WORDS: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    /// Language code(s), e.g. "de+en".
    pub lang: String,
    pub name: String,
    /// Model file (relative to the pack), or none for a pack without a recogniser.
    pub model: Option<String>,
    /// Character list, one per line.
    #[serde(default)]
    pub dict: Option<String>,
    /// Version stored with recognised text, so blocks are re-read after an upgrade.
    #[serde(default)]
    pub pack_version: Option<String>,
    /// Word lexicon for beam decoding: `word<TAB>score` per line (ADR 0006 step (a)).
    #[serde(default)]
    pub lexicon: Option<String>,
    /// Beam decoder weights (used with the lexicon).
    #[serde(default)]
    pub decoder: Option<crate::ctc::BeamParams>,
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pack {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Pack {
    fn file(&self, name: &str) -> Result<PathBuf, String> {
        // Only plain file names inside the pack directory.
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
            return Err(format!("pack {}: bad file name {name:?}", self.manifest.lang));
        }
        Ok(self.dir.join(name))
    }

    pub fn model_path(&self) -> Result<Option<PathBuf>, String> {
        self.manifest.model.as_deref().map(|m| self.file(m)).transpose()
    }

    /// The character list (without blank and space classes).
    pub fn chars(&self) -> Result<Vec<String>, String> {
        let Some(d) = self.manifest.dict.as_deref() else {
            return Ok(Vec::new());
        };
        let p = self.file(d)?;
        let len = std::fs::metadata(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?
            .len();
        if len > MAX_DICT {
            return Err(format!("{}: too large", p.display()));
        }
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(s.lines().map(str::to_string).collect())
    }

    /// The word lexicon (empty without one). Lines that do not parse are skipped.
    pub fn lexicon(&self) -> Result<crate::ctc::Lexicon, String> {
        let Some(l) = self.manifest.lexicon.as_deref() else {
            return Ok(crate::ctc::Lexicon::default());
        };
        let p = self.file(l)?;
        let len = std::fs::metadata(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?
            .len();
        if len > MAX_LEXICON {
            return Err(format!("{}: too large", p.display()));
        }
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut lex = crate::ctc::Lexicon::default();
        for line in s.lines().take(MAX_WORDS) {
            if let Some((w, v)) = line.split_once('\t')
                && let Ok(v) = v.trim().parse::<f32>()
                && v.is_finite()
                && !w.is_empty()
            {
                lex.insert(w, v);
            }
        }
        Ok(lex)
    }

    /// The version tag stored with recognised text.
    pub fn tag(&self) -> String {
        format!(
            "{}@{}",
            self.manifest.lang,
            self.manifest.pack_version.as_deref().unwrap_or("0")
        )
    }
}

/// Reads one pack directory.
pub fn load(dir: &Path) -> Result<Pack, String> {
    let m = dir.join("manifest.json");
    let len = std::fs::metadata(&m)
        .map_err(|e| format!("{}: {e}", m.display()))?
        .len();
    if len > MAX_MANIFEST {
        return Err(format!("{}: too large", m.display()));
    }
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(&m).map_err(|e| format!("{}: {e}", m.display()))?)
            .map_err(|e| format!("{}: {e}", m.display()))?;
    if manifest.format != "vibenote-language-pack" || manifest.version != 1 {
        return Err(format!("{}: not a version-1 language pack", m.display()));
    }
    if manifest.lang.is_empty() || manifest.lang.len() > 32 || manifest.name.len() > 256 {
        return Err(format!("{}: bad lang or name", m.display()));
    }
    Ok(Pack {
        dir: dir.to_path_buf(),
        manifest,
    })
}

/// Every pack in the given directories (each a pack, or a directory of packs); unreadable
/// ones are reported, not fatal.
pub fn discover(dirs: &[PathBuf]) -> (Vec<Pack>, Vec<String>) {
    let mut packs = Vec::new();
    let mut errors = Vec::new();
    for d in dirs {
        if d.join("manifest.json").is_file() {
            match load(d) {
                Ok(p) => packs.push(p),
                Err(e) => errors.push(e),
            }
            continue;
        }
        let Ok(rd) = std::fs::read_dir(d) else {
            errors.push(format!("{}: not a directory", d.display()));
            continue;
        };
        let mut subs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("manifest.json").is_file())
            .collect();
        subs.sort();
        for s in subs.into_iter().take(1000) {
            match load(&s) {
                Ok(p) => packs.push(p),
                Err(e) => errors.push(e),
            }
        }
    }
    (packs, errors)
}
