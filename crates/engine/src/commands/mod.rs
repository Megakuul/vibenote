//! Command implementations and registration.

pub(crate) mod audio;
mod blob;
mod edit;
pub(crate) mod hwr;
mod image;
mod ink;
pub(crate) mod input;
pub(crate) mod keymap;
mod link;
mod order;
pub(crate) mod pdf;
mod quickopen;
mod search;
pub(crate) mod selection;
mod settings;
pub(crate) mod shape;
mod stubs;
pub(crate) mod text;
mod trash;
mod ui;
pub(crate) mod view;
mod workspace;

use crate::EngineError;
use crate::command::Registry;

pub fn register_all(r: &mut Registry) -> Result<(), EngineError> {
    workspace::register(r)?;
    ink::register(r)?;
    input::register(r)?;
    edit::register(r)?;
    settings::register(r)?;
    selection::register(r)?;
    order::register(r)?;
    pdf::register(r)?;
    quickopen::register(r)?;
    keymap::register(r)?;
    ui::register(r)?;
    trash::register(r)?;
    view::register(r)?;
    blob::register(r)?;
    text::register(r)?;
    link::register(r)?;
    shape::register(r)?;
    image::register(r)?;
    search::register(r)?;
    hwr::register(r)?;
    audio::register(r)?;
    stubs::register(r)?;
    Ok(())
}
