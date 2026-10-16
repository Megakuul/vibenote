//! Keymap (R-CMD-002): shortcuts map to command ids; every binding is rebindable. Defaults
//! follow SPEC §14; changes are stored as overrides (`keymap` in the workspace meta).
//!
//! Keys are written `Mod+…+Key` with modifiers in the order Ctrl, Alt, Shift, Super; letters are
//! upper case ("Ctrl+Shift+Z"); named keys: Delete, Backspace, Escape, Enter, Tab, Space, Plus,
//! Minus, Equal, Up, Down, Left, Right, Home, End, PageUp, PageDown, F1–F12; `Space+Drag` is
//! the pan gesture.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default bindings (SPEC R-CMD-002 plus zoom keys, Escape and Ctrl+Q).
pub const DEFAULTS: &[(&str, &str)] = &[
    ("P", "tool.select.pen"),
    ("H", "tool.select.highlighter"),
    ("E", "tool.select.eraser"),
    ("L", "tool.select.lasso"),
    ("S", "tool.select.shape"),
    ("R", "ruler.toggle"),
    ("M", "tool.select.cover"),
    ("Alt+Left", "ruler.rotate_left"),
    ("Alt+Right", "ruler.rotate_right"),
    ("T", "tool.select.text"),
    ("Ctrl+Z", "edit.undo"),
    ("Ctrl+Shift+Z", "edit.redo"),
    ("Ctrl+C", "clipboard.copy"),
    ("Ctrl+X", "clipboard.cut"),
    ("Ctrl+V", "clipboard.paste"),
    ("Delete", "selection.delete"),
    ("Backspace", "selection.delete"),
    ("Escape", "selection.clear"),
    ("Ctrl+P", "ui.quickopen.show"),
    ("Ctrl+F", "ui.find.show"),
    ("Ctrl+Shift+F", "ui.search.show"),
    ("Ctrl+0", "view.zoom_fit"),
    ("Ctrl+1", "view.zoom_reset"),
    ("Ctrl+Plus", "view.zoom_in"),
    ("Ctrl+Equal", "view.zoom_in"),
    ("Ctrl+Minus", "view.zoom_out"),
    ("Ctrl+Q", "app.quit"),
    ("Space+Drag", "view.pan"),
];

/// Most overrides kept (hostile stored data).
const MAX_OVERRIDES: usize = 1024;
const MODS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];
const NAMED: &[&str] = &[
    "Delete",
    "Backspace",
    "Escape",
    "Enter",
    "Tab",
    "Space",
    "Plus",
    "Minus",
    "Equal",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

/// The canonical form of a key ("ctrl+shift+z" → "Ctrl+Shift+Z"), or an error.
pub fn canonical(key: &str) -> Result<String, EngineError> {
    let bad = || EngineError::Model(format!("unknown key {key:?}"));
    if key.len() > 64 {
        return Err(bad());
    }
    let parts: Vec<&str> = key.split('+').map(str::trim).collect();
    let (last, mods) = parts.split_last().ok_or_else(bad)?;
    if last.eq_ignore_ascii_case("drag") {
        return if mods.len() == 1 && mods.first().is_some_and(|m| m.eq_ignore_ascii_case("space")) {
            Ok("Space+Drag".into())
        } else {
            Err(bad())
        };
    }
    let mut have = [false; 4];
    for m in mods {
        let i = MODS.iter().position(|x| x.eq_ignore_ascii_case(m)).ok_or_else(bad)?;
        let slot = have.get_mut(i).ok_or_else(bad)?;
        if *slot {
            return Err(bad());
        }
        *slot = true;
    }
    let mut chars = last.chars();
    let name = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
        _ => {
            let fkey = last
                .strip_prefix(['F', 'f'])
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=12).contains(n));
            match fkey {
                Some(n) => format!("F{n}"),
                None => NAMED
                    .iter()
                    .find(|n| n.eq_ignore_ascii_case(last))
                    .map(|n| (*n).to_string())
                    .ok_or_else(bad)?,
            }
        }
    };
    let mut out: Vec<String> = MODS
        .iter()
        .zip(have)
        .filter(|(_, h)| *h)
        .map(|(m, _)| (*m).to_string())
        .collect();
    out.push(name);
    Ok(out.join("+"))
}

/// The effective bindings: defaults with the overrides applied.
pub fn bindings(s: &Session) -> BTreeMap<String, String> {
    let mut b: BTreeMap<String, String> = DEFAULTS
        .iter()
        .map(|(k, c)| ((*k).to_string(), (*c).to_string()))
        .collect();
    for (k, c) in &s.keymap_overrides {
        match c {
            Some(c) => b.insert(k.clone(), c.clone()),
            None => b.remove(k),
        };
    }
    b
}

/// Keeps the valid overrides of stored data (known key syntax, registered commands); returns
/// the dropped keys.
pub(crate) fn load_overrides(s: &mut Session, stored: BTreeMap<String, Option<String>>) -> Vec<String> {
    let mut dropped = Vec::new();
    for (k, c) in stored {
        let ok_cmd = c.as_deref().is_none_or(|c| s.command_ids.contains(&c));
        match canonical(&k) {
            Ok(key) if ok_cmd && s.keymap_overrides.len() < MAX_OVERRIDES => {
                s.keymap_overrides.insert(key, c);
            }
            _ => dropped.push(k),
        }
    }
    dropped
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetP {
    key: String,
    /// Command id; null unbinds the key.
    command: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResetP {
    /// Key to restore to its default; omitted: all keys.
    key: Option<String>,
}

#[derive(Serialize)]
struct Keymap {
    bindings: BTreeMap<String, String>,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "keymap.get",
            doc: "The effective shortcuts: key → command id.",
            reqs: &["R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| Ok(Keymap { bindings: bindings(s) }),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "keymap.set",
            doc: "Bind a key to a command (or unbind it with null); stored with the workspace.",
            reqs: &["R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, p: SetP| {
            let key = canonical(&p.key)?;
            if let Some(c) = &p.command
                && !s.command_ids.contains(&c.as_str())
            {
                return Err(EngineError::Model(format!("unknown command {c}")));
            }
            let default = DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, c)| *c);
            if p.command.as_deref() == default {
                s.keymap_overrides.remove(&key);
            } else {
                if s.keymap_overrides.len() >= MAX_OVERRIDES && !s.keymap_overrides.contains_key(&key) {
                    return Err(EngineError::Model(format!("at most {MAX_OVERRIDES} changed keys")));
                }
                s.keymap_overrides.insert(key, p.command);
            }
            s.meta_dirty.insert("keymap");
            Ok(Keymap { bindings: bindings(s) })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "keymap.reset",
            doc: "Restore a key (or all keys) to the default binding.",
            reqs: &["R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, p: ResetP| {
            match p.key {
                Some(k) => {
                    s.keymap_overrides.remove(&canonical(&k)?);
                }
                None => s.keymap_overrides.clear(),
            }
            s.meta_dirty.insert("keymap");
            Ok(Keymap { bindings: bindings(s) })
        },
    )))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn keys_normalise_and_bad_keys_are_rejected() {
        assert_eq!(canonical("shift+ctrl+z").unwrap(), "Ctrl+Shift+Z");
        assert_eq!(canonical("Ctrl + 0").unwrap(), "Ctrl+0");
        assert_eq!(canonical("delete").unwrap(), "Delete");
        assert_eq!(canonical("f12").unwrap(), "F12");
        assert_eq!(canonical("space+drag").unwrap(), "Space+Drag");
        for bad in [
            "",
            "Ctrl+",
            "Ctrl+Ctrl+Z",
            "Hyper+Z",
            "F13",
            "Ctrl+Drag",
            "ZZ",
            "Ctrl+ä",
        ] {
            assert!(canonical(bad).is_err(), "{bad}");
        }
        assert!(canonical(&"Ctrl+".repeat(40)).is_err());
        for (k, _) in DEFAULTS {
            assert_eq!(canonical(k).unwrap(), *k, "defaults are canonical");
        }
    }
}
