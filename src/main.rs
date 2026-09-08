fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|arg| arg == "--workenv-exec-with-stdin")
    {
        workenv::process::exec_with_stdin(args.collect())?;
        return Ok(());
    }
    // Incurs stdio tool calls do not inherit parsed CLI globals. Keep the
    // server's explicit root as process-local configuration for its handlers.
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .collect();
    if args.iter().any(|arg| arg == "--mcp") {
        for (index, arg) in args.iter().enumerate() {
            let root = if arg == "--root" {
                args.get(index + 1).map(std::path::PathBuf::from)
            } else {
                arg.to_str()
                    .and_then(|arg| arg.strip_prefix("--root="))
                    .map(std::path::PathBuf::from)
            };
            if let Some(root) = root {
                workenv::process::set_server_root(root)?;
                break;
            }
        }
    }
    serve()
}

#[tokio::main]
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    workenv::cli::build().serve().await
}
