//! Placeholder for operating systems without a back-end yet. The daemon
//! builds and its control interface works, but it cannot share input.

use std::sync::Arc;

use anyhow::{bail, Result};
use yzendris_core::layout::Screen;
use yzendris_core::Point;

use super::{Inject, Sink};

pub const LOG_TO_FILE: bool = false;

pub fn prepare() {}

pub fn local_clock() -> (u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (
        (secs / 3600 % 24) as u32,
        (secs / 60 % 60) as u32,
        (secs % 60) as u32,
    )
}

pub fn screens() -> Vec<Screen> {
    Vec::new()
}

pub fn injector(_layout: &str) -> Result<Box<dyn Inject>> {
    bail!("this operating system is not supported yet")
}

pub struct Capture;

impl Capture {
    pub fn start(_sink: Arc<dyn Sink>) -> Result<Self> {
        bail!("this operating system is not supported yet")
    }
    pub fn set_capture(&self, _on: bool) {}
    pub fn warp(&self, _to: Point) {}
    pub fn sees_cursor(&self) -> bool {
        false
    }
}
