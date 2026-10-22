use std::time::Duration;

/// What a `Retry-After` header said, kept in the form the server sent it. The delta form
/// needs no clock, and the date form is resolved against one when the pacer applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfter {
    After(Duration),
    AtUnixSecs(i64),
}

impl RetryAfter {
    /// Delta-seconds or an IMF-fixdate, per RFC 9110 section 10.2.3. Anything else
    /// counts as no header.
    pub fn parse(header: &str) -> Option<Self> {
        let value = header.trim();
        if value.is_empty() {
            return None;
        }
        if value.bytes().all(|byte| byte.is_ascii_digit()) {
            return value.parse::<u64>().ok().map(|secs| Self::After(Duration::from_secs(secs)));
        }
        imf_fixdate_unix_secs(value).map(Self::AtUnixSecs)
    }

    /// How long to wait, given the current wall clock. A date already past
    /// asks for no wait.
    pub fn resolve(self, now_unix_secs: i64) -> Duration {
        match self {
            Self::After(delay) => delay,
            Self::AtUnixSecs(at_unix_secs) => {
                u64::try_from(at_unix_secs.saturating_sub(now_unix_secs))
                    .map_or(Duration::ZERO, Duration::from_secs)
            }
        }
    }
}

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// An IMF-fixdate such as `Sun, 06 Nov 1994 08:49:37 GMT`, in unix seconds.
///
/// It is read leniently: the weekday is not checked, field widths are not enforced, the
/// second is not range-checked, and any month takes a day up to 31.
fn imf_fixdate_unix_secs(value: &str) -> Option<i64> {
    let (_, rest) = value.split_once(", ")?;
    let mut fields = rest.split(' ');
    let day: i64 = fields.next()?.parse().ok()?;
    let month_name = fields.next()?;
    let year: i64 = fields.next()?.parse().ok()?;
    let time = fields.next()?;
    if fields.next() != Some("GMT") {
        return None;
    }
    let month = i64::try_from(MONTHS.iter().position(|name| *name == month_name)?).ok()?;
    let mut time_fields = time.split(':');
    let hour: i64 = time_fields.next()?.parse().ok()?;
    let minute: i64 = time_fields.next()?.parse().ok()?;
    let second: i64 = time_fields.next()?.parse().ok()?;
    if time_fields.next().is_some() || !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    let days = days_from_civil(year, month.checked_add(1)?, day)?;
    days.checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date, Howard Hinnant's
/// `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year.checked_sub(1)? } else { year };
    let era = if year >= 0 { year } else { year.checked_sub(399)? }.checked_div(400)?;
    let year_of_era = year.checked_sub(era.checked_mul(400)?)?;
    let month_shift = if month > 2 { month.checked_sub(3)? } else { month.checked_add(9)? };
    let day_of_year = month_shift
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day)?
        .checked_sub(1)?;
    let day_of_era = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era.checked_div(4)?)?
        .checked_sub(year_of_era.checked_div(100)?)?
        .checked_add(day_of_year)?;
    era.checked_mul(146_097)?.checked_add(day_of_era)?.checked_sub(719_468)
}
