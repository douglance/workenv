//! Workenv Tailscale adapter binary.

fn main() -> anyhow::Result<()> {
    workenv_protocol::serve(workenv_adapter_tailscale::handle)
}
