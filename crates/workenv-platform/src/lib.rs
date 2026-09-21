//! Host primitives used by the generic Workenv controller.
mod execution;
mod execution_code;
mod execution_path;
mod json_store;
mod shell;

pub use execution::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};
pub use execution_path::locate_executable;
pub use json_store::{read_json, with_exclusive_lock, write_json_atomic};
pub use shell::{shell_join, shell_quote};
