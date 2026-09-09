//! Lima ephemeral worker provider adapter entrypoint.
mod provider;

use anyhow::Result;
use provider::handle;
use workenv_protocol::serve;

fn main() -> Result<()> {
    serve(handle)
}
