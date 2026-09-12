use super::*;

#[test]
fn unparsable_stdout_names_the_execution_and_both_streams() {
    let message = output_message(&ExecutionOutput {
        stdout: "herdr: unknown subcommand status".to_string(),
        stderr: "usage: herdr status server".to_string(),
        exit_code: Some(0),
        execution_id: "execution-7".to_string(),
    });
    assert!(message.contains("execution-7"), "{message}");
    assert!(message.contains("unknown subcommand"), "{message}");
    assert!(message.contains("usage: herdr status server"), "{message}");
}

#[test]
fn unparsable_stdout_keeps_both_excerpts_bounded() {
    let message = output_message(&ExecutionOutput {
        stdout: "s".repeat(20_000),
        stderr: "e".repeat(20_000),
        exit_code: Some(0),
        execution_id: "execution-8".to_string(),
    });
    assert!(
        message.len() < 1_000,
        "excerpt is unbounded at {} bytes",
        message.len()
    );
}

fn output_message(output: &ExecutionOutput) -> String {
    output_json(output)
        .err()
        .map_or_else(String::new, |error| format!("{error:#}"))
}
