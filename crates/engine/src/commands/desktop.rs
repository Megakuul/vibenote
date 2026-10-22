//! `app.install_desktop_entry` (R-PLAT-004): adds vibenote to the app launcher for this user.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstallP {
    /// The program the entry starts (default: the AppImage, the package's launcher, or this
    /// executable).
    launcher: Option<String>,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "app.install_desktop_entry",
            doc: "Add vibenote to the app launcher: a desktop entry and icons in $XDG_DATA_HOME (not over a system-wide entry or one the user wrote).",
            reqs: &["R-PLAT-004"],
            implemented: true,
        },
        |_: &mut Session, p: InstallP| -> Result<Value, EngineError> {
            let launcher = p
                .launcher
                .map(std::path::PathBuf::from)
                .or_else(crate::desktop::launcher)
                .ok_or_else(|| EngineError::Model("cannot tell which program to start".into()))?;
            let dirs = crate::desktop::Dirs::from_env()
                .ok_or_else(|| EngineError::Model("neither XDG_DATA_HOME nor HOME is set".into()))?;
            let out = crate::desktop::install(&dirs, &launcher).map_err(EngineError::Model)?;
            Ok(json!({"result": out, "message": out.message()}))
        },
    )))?;
    Ok(())
}
