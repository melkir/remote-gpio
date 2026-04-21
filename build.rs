use anyhow::Result;
use vergen_gix::{BuildBuilder, Emitter, GixBuilder};

pub fn main() -> Result<()> {
    Emitter::default()
        .add_instructions(&BuildBuilder::all_build()?)?
        .add_instructions(&GixBuilder::all_git()?)?
        .emit()
}
