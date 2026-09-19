//! Turning engine numbers into the strings the window shows.

use crate::i18n::Lang;

/// Human-readable byte size in the language's decimal style: `1,4 GB` in
/// Turkish, `1.4 GB` in English.
pub fn size(lang: Lang, bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    // Three significant figures read best in a column: 9.87 GB, 98.7 GB, 987 GB.
    let text = if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    let text = match lang {
        Lang::Tr => text.replace('.', ","),
        Lang::En => text,
    };
    format!("{text} {}", UNITS[unit])
}

/// Share of a whole, e.g. `12.3%` / `%12,3`.
pub fn percent(lang: Lang, part: u64, whole: u64) -> String {
    let value = part as f64 * 100.0 / whole.max(1) as f64;
    match lang {
        Lang::Tr => format!("%{}", format!("{value:.1}").replace('.', ",")),
        Lang::En => format!("{value:.1}%"),
    }
}

/// A Windows FILETIME as a date, `YYYY-MM-DD`, in UTC.
///
/// The day is all a disk-usage view needs: "last touched three years ago" is
/// the question, not the minute.
pub fn date(filetime: u64) -> String {
    let Some(unix) = ferret_core::filetime_to_unix(filetime) else {
        return String::new();
    };
    if unix < 0 {
        return String::new();
    }
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_keep_three_significant_figures() {
        assert_eq!(size(Lang::En, 512), "512 B");
        assert_eq!(size(Lang::En, 1536), "1.50 KB");
        assert_eq!(size(Lang::En, 25 << 30), "25.0 GB");
        assert_eq!(size(Lang::En, 400 << 30), "400 GB");
        assert_eq!(size(Lang::Tr, 1536), "1,50 KB");
    }

    #[test]
    fn percentages_follow_the_language() {
        assert_eq!(percent(Lang::En, 1, 8), "12.5%");
        assert_eq!(percent(Lang::Tr, 1, 8), "%12,5");
        assert_eq!(percent(Lang::En, 1, 0), "100.0%");
    }

    #[test]
    fn dates_survive_leap_days() {
        assert_eq!(date(116_444_736_000_000_000), "1970-01-01");
        assert_eq!(date(133_536_879_000_000_000), "2024-02-29");
        assert_eq!(date(125_963_423_400_000_000), "2000-02-29");
        assert_eq!(date(0), "");
    }
}
