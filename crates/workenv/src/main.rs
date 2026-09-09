//! Workenv executable entrypoint.
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.iter().any(|arg| arg == "--mcp")
        && let Some(root) = explicit_root(&arguments)
    {
        workenv::set_server_root(&root)?;
    }
    workenv::build().serve().await
}

fn explicit_root(arguments: &[std::ffi::OsString]) -> Option<PathBuf> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        if argument == "--root" {
            arguments.get(index + 1).map(PathBuf::from)
        } else {
            argument
                .to_str()
                .and_then(|text| text.strip_prefix("--root="))
                .map(PathBuf::from)
        }
    })
}
