//! Workenv Herdr adapter binary.

fn main() -> anyhow::Result<()> {
    workenv_protocol::serve(workenv_adapter_herdr::handle)
}
