use anyhow::Result;
use vergen_gix::{Build, Emitter, Gix};

pub fn main() -> Result<()> {
    Emitter::default()
        .add_instructions(&Build::all_build())?
        .add_instructions(&Gix::all_git())?
        .emit()
}
