use std::{str::FromStr, time::Duration};

fn seconds(whole: &str) -> Option<u32> {
    let mut iter = whole.rsplitn(3, ':');
    let seconds = iter.next().unwrap();
    let minutes = iter.next();
    let hours = iter.next();

    if iter.next().is_some() {
        return None;
    }

    fn min_len(has_next: bool) -> usize {
        if has_next {
            2
        } else {
            1
        }
    }

    if seconds.len() > 2 || seconds.len() < min_len(minutes.is_some()) {
        return None;
    }
    let seconds = u32::from_str(seconds).ok()?;
    if seconds >= 60 {
        return None;
    }

    let minutes = minutes.unwrap_or("0");
    if minutes.len() > 2 || minutes.len() < min_len(hours.is_some()) {
        return None;
    }
    let minutes = u32::from_str(minutes).ok()?;
    if minutes >= 60 {
        return None;
    }

    let hours = hours.unwrap_or("0");
    if hours.len() > 2 || hours.is_empty() {
        return None;
    }
    let hours = u32::from_str(hours).ok()?;

    Some(hours * 3600 + minutes * 60 + seconds)
}

fn milliseconds(fractional: &str) -> Option<u32> {
    let mut iter = fractional.chars();
    let h = iter.next()?.to_digit(10)?;
    let t = iter.next().unwrap_or('0').to_digit(10)?;
    let u = iter.next().unwrap_or('0').to_digit(10)?;

    if iter.next().is_some() {
        return None;
    }

    Some(h * 100 + t * 10 + u)
}

pub fn timestamp(input: &str) -> Option<u32> {
    let (whole, fractional) = input.split_once('.').unwrap_or((input, "0"));

    let seconds = seconds(whole)?;
    let milliseconds = milliseconds(fractional)?;

    Some(seconds * 1000 + milliseconds)
}

pub fn time_to_entry_text(time: Duration) -> String {
    let time = Duration::from_millis((time.as_millis() as f64 / 100.).round() as u64 * 100);
    let mut seconds = time.as_secs();
    let mut minutes = seconds / 60;
    let hours = minutes / 60;
    seconds %= 60;
    minutes %= 60;

    let fractional = (time.subsec_nanos() / 100_000_000) % 10;

    if hours == 0 {
        format!("{}:{:02}.{}", minutes, seconds, fractional)
    } else {
        format!("{}:{:02}:{:02}.{}", hours, minutes, seconds, fractional)
    }
}

pub fn time_for_filename(time: &str) -> &str {
    if !time.contains('.') {
        return time;
    }
    let trimmed = time.trim_end_matches('0');
    // If all fractional digits were zero, strip the dot too.
    trimmed.strip_suffix('.').unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp() {
        assert_eq!(timestamp("1"), Some(1000));
        assert_eq!(timestamp("1:23:45.678"), Some(5025678));
        assert_eq!(timestamp("2.43"), Some(2430));
        assert_eq!(timestamp("1."), None);
        assert_eq!(timestamp("60"), None);
        assert_eq!(timestamp(":3"), None);
        assert_eq!(timestamp("2:"), None);
        assert_eq!(timestamp("2:03"), Some(123000));
        assert_eq!(timestamp("2:3"), None);
        assert_eq!(timestamp("99:00:00"), Some(356400000));
        assert_eq!(timestamp("100:00:00"), None);
        assert_eq!(timestamp("1:02:03:04"), None);
        assert_eq!(timestamp("1.2.3"), None);
        assert_eq!(timestamp("1.2345"), None);
        assert_eq!(timestamp(""), None);
    }

    #[test]
    fn test_time_to_entry_text() {
        assert_eq!(&time_to_entry_text(Duration::from_millis(1234)), "0:01.2");
        assert_eq!(&time_to_entry_text(Duration::from_millis(2000)), "0:02.0");
        assert_eq!(&time_to_entry_text(Duration::from_millis(67890)), "1:07.9");
        assert_eq!(
            &time_to_entry_text(Duration::from_millis(3600000)),
            "1:00:00.0"
        );
    }

    #[test]
    fn test_time_for_filename() {
        assert_eq!(time_for_filename("0:32.0"), "0:32");
        assert_eq!(time_for_filename("1:05.300"), "1:05.3");
        assert_eq!(time_for_filename("1:05.2"), "1:05.2");
        assert_eq!(time_for_filename("1:05"), "1:05");
        assert_eq!(time_for_filename("1:00:00.000"), "1:00:00");
        assert_eq!(time_for_filename("10:00.0"), "10:00");
        assert_eq!(time_for_filename("0:00.0"), "0:00");
    }
}
