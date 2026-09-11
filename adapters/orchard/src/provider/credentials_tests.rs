//! Controller credential parsing tests.
use super::credentials::parse;

#[test]
fn a_token_containing_a_colon_space_survives_intact() {
    // This is why the format is two lines rather than orchard's YAML: a
    // line-oriented YAML reader splits on ": " and would hand the controller a
    // truncated secret, which fails as a wrong password rather than as a
    // parsing bug.
    let found = parse("bootstrap-admin\nab: cd: ef\n").expect("credentials");
    assert_eq!(found.token, "ab: cd: ef");
}

#[test]
fn a_generated_token_keeps_every_metacharacter() {
    // Synthetic, drawn from the alphabet the controller actually generates
    // from: backslash, braces, quotes and shell metacharacters. A real token
    // must never be a test fixture, because fixtures get committed.
    let raw = "Zz09\\8Q7(,=aB!^&*35X0R6u9}]'\"";
    let found = parse(&format!("acct\n{raw}\n")).expect("credentials");
    assert_eq!(found.token, raw);
}

#[test]
fn interior_spaces_are_not_trimmed_away() {
    // Trimming the token would send a different secret than was stored, and the
    // 401 that follows names the account, not the whitespace.
    let found = parse("acct\n  padded token  \n").expect("credentials");
    assert_eq!(found.token, "  padded token  ");
}

#[test]
fn a_missing_token_is_no_credentials_rather_than_an_empty_one() {
    // Sending Basic auth for an empty password reads as a wrong password. The
    // adapter must instead behave as if it has none, which is a legitimate
    // state against a controller that has no service accounts yet.
    assert!(parse("acct\n").is_none());
    assert!(parse("acct\n\n").is_none());
}

#[test]
fn a_missing_account_is_refused_too() {
    assert!(parse("\ntoken-only").is_none());
}

#[test]
fn a_file_with_no_newline_is_not_half_read() {
    // One line cannot carry both fields, and guessing which one it is would
    // authenticate as the token, or with the account as the password.
    assert!(parse("just-an-account").is_none());
}
