//! Orchard cluster provider adapter entrypoint.
mod provider;

use anyhow::Result;
use provider::handle;
use workenv_protocol::serve;

fn main() -> Result<()> {
    // `serve` takes a fallible handler; this adapter turns every failure into a
    // Failed response instead, so the conversion happens here rather than
    // wrapping every return in an Ok that can never be an Err.
    serve(|request| Ok(handle(request)))
}
