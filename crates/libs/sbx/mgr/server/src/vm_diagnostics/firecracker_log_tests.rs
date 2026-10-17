use super::*;

fn line(level: &str, message: &str) -> String {
    format!("2026-01-01T00:00:00.000000000 [anonymous-instance:fc_vcpu 0:{level}] {message}\n")
}

/// Firecracker's SIGPIPE line is routine and can arrive in floods, so it must
/// not spend the sandbox's re-log allowance: every real ERROR line around it
/// is still forwarded, and only those lines draw on the budget.
#[test]
fn sigpipe_lines_do_not_spend_the_budget_real_errors_need() {
    let mut chunk = String::new();
    for index in 0..LogBudget::BURST {
        for _ in 0..50 {
            chunk.push_str(&line("ERROR", "Received signal 13, code 0."));
        }
        chunk.push_str(&line("WARN", "routine warning"));
        // Messages that only resemble the SIGPIPE line are real errors.
        let real = match index % 3 {
            0 => "Shutting down VM after intercepting signal 7, code 2.".to_string(),
            1 => "Received signal 130, code 0.".to_string(),
            _ => format!("vsock: failed to write to host, error {index}"),
        };
        chunk.push_str(&line("ERROR", &real));
    }
    let mut tail = LogTail::new(PathBuf::from("unused"), SystemTime::now());
    let mut budget = LogBudget::new();

    tail.accept("sandbox-a", chunk.as_bytes(), &mut budget);

    assert_eq!(budget.tokens, 0, "every real ERROR line drew on the budget");
    assert_eq!(budget.suppressed, 0, "no real ERROR line was rate limited");
}
