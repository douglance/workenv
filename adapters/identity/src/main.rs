//! Workenv identity adapter binary and Nib proxy entrypoint.

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "nib-proxy") {
        return workenv_adapter_identity::nib::proxy(&args.split_off(2));
    }
    if args.get(1).is_some_and(|arg| arg == "nib-store") {
        return workenv_adapter_identity::nib::store_from_stdin();
    }
    workenv_protocol::serve(workenv_adapter_identity::handle)
}
