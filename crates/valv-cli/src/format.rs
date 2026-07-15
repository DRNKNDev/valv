use chrono::{DateTime, Utc};

pub(crate) fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit_index = 0;
    while (value * 10.0).round() / 10.0 >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }
    format!("{value:.1} {}", UNITS[unit_index])
}

pub(crate) fn age_from_now(created_at: &str) -> Option<String> {
    let created = DateTime::parse_from_rfc3339(created_at)
        .ok()?
        .with_timezone(&Utc);
    Some(humanize_duration(Utc::now().signed_duration_since(created)))
}

pub(crate) fn humanize_duration(delta: chrono::Duration) -> String {
    let seconds = delta.num_seconds().max(0);
    if seconds < 60 {
        return plural(seconds, "second");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return plural(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return plural(hours, "hour");
    }
    plural(hours / 24, "day")
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_bytes_renders_whole_bytes_below_one_kb() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1023), "1023 B");
    }

    #[test]
    fn humanize_bytes_renders_one_decimal_place_from_kb_up() {
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(humanize_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn humanize_bytes_renders_a_value_requiring_a_decimal() {
        assert_eq!(humanize_bytes(1536), "1.5 KB");
        assert_eq!(humanize_bytes(2 * 1024 * 1024 + 512 * 1024), "2.5 MB");
    }

    #[test]
    fn humanize_bytes_promotes_a_just_below_boundary_value_after_rounding() {
        assert_eq!(humanize_bytes(1024 * 1024 - 1), "1.0 MB");
        assert_eq!(humanize_bytes(1024 * 1024 * 1024 - 1), "1.0 GB");
    }

    #[test]
    fn humanize_duration_names_minutes() {
        assert_eq!(humanize_duration(chrono::Duration::minutes(4)), "4 minutes ago");
        assert_eq!(humanize_duration(chrono::Duration::minutes(1)), "1 minute ago");
        assert_eq!(humanize_duration(chrono::Duration::seconds(30)), "30 seconds ago");
    }
}
