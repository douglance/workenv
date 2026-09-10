//! Project checkout lifecycle adapter binary.

use anyhow::Result;

fn main() -> Result<()> {
    workenv_adapter_project::serve()
}
