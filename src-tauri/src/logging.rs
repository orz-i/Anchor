use chrono::{SecondsFormat, Utc};

pub(crate) fn timestamped_line(line: &str) -> String {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    format!("[{timestamp}] {line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_uses_parseable_utc_rfc3339_timestamp() {
        let line = timestamped_line("[mcp] started");
        let (timestamp, message) = line
            .strip_prefix('[')
            .and_then(|value| value.split_once("] "))
            .expect("timestamp prefix");

        assert!(timestamp.ends_with('Z'));
        assert!(chrono::DateTime::parse_from_rfc3339(timestamp).is_ok());
        assert_eq!(message, "[mcp] started");
    }
}
