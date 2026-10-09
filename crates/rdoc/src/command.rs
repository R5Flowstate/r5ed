//! Every edit is a `Command`; applying one returns its inverse, which is what undo replays.

use serde::{Deserialize, Serialize};

use crate::model::{Id, Layer, MapMeta, Object, Settings};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    AddObjects { objects: Vec<Object> },
    RemoveObjects { ids: Vec<Id> },
    /// Replaces whole objects (the inverse carries the previous values).
    ReplaceObjects { objects: Vec<Object> },
    AddLayer { layer: Layer },
    RemoveLayer { id: Id },
    ReplaceLayer { layer: Layer },
    SetMeta { meta: MapMeta },
    SetSettings { settings: Settings },
    /// Writes or deletes (None) a project file such as terrain heights.
    SetFile { path: String, bytes: Option<Vec<u8>> },
    Batch { label: String, commands: Vec<Command> },
}
