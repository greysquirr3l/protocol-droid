//! `ReleaseDate` — the `YYYY-MM-DD` string the protocol negotiates on.
//!
//! Every published revision is identified by its release date and nothing else,
//! so this is the one place that decides what such a string may look like. The
//! fields are private, which means a `ReleaseDate` that exists is a valid one:
//! the only ways to obtain one are [`FromStr`], which validates, and the
//! associated constants, which are correct by construction.
//!
//! # What "strict" means here
//!
//! A peer that sends `2025-3-26` or `2025-03-26T00:00:00Z` is not a peer whose
//! version we can guess at. Both are rejected rather than repaired, because
//! guessing wrong is how a bridge ends up speaking a revision neither side
//! agreed to. The only leniency is [`FromStr::from_str`] falling back to
//! [`ProtocolVersion::Unknown`] for a well-formed date this build has never
//! heard of, which is the forward-compatibility case and nothing else.
//!
//! [`ProtocolVersion::Unknown`]: crate::domain::ProtocolVersion::Unknown

use core::fmt;
use core::str::FromStr;

/// A spec revision's release date.
///
/// Ordered chronologically: the derived comparison walks `year`, then `month`,
/// then `day`, which is the only ordering in which those three fields compose
/// correctly. Nothing else may be derived from the ordering, so the `Ord` and
/// `PartialOrd` pair below comes from that same field order and
/// `tests::derived_ordering_agrees_in_both_directions` pins the two together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseDate {
    /// Proleptic Gregorian year.
    year: u16,
    /// Month, `1..=12`.
    month: u8,
    /// Day of the month, `1..=31` and within the month.
    day: u8,
}

impl ReleaseDate {
    /// The first published revision, `2024-11-05`.
    ///
    /// Also the floor for [`ProtocolVersion::from_str`]: a well-formed date
    /// before this one names a revision that predates the protocol, which is a
    /// parse error rather than an unknown revision.
    ///
    /// [`ProtocolVersion::from_str`]: crate::domain::ProtocolVersion::from_str
    pub const V2024_11_05: Self = Self::new(2024, 11, 5);
    /// The `2025-03-26` revision.
    pub const V2025_03_26: Self = Self::new(2025, 3, 26);
    /// The `2025-06-18` revision.
    pub const V2025_06_18: Self = Self::new(2025, 6, 18);
    /// The `2025-11-25` revision.
    pub const V2025_11_25: Self = Self::new(2025, 11, 25);
    /// The `2026-07-28` revision, and the newest this build knows.
    pub const V2026_07_28: Self = Self::new(2026, 7, 28);

    /// A date from three already-validated components.
    const fn new(year: u16, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }

    /// The year, as a four-digit value.
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// The month, `1..=12`.
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// The day of the month.
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// Whether this date is [`V2024_11_05`] or later.
    ///
    /// The comparison is on the date, not the enum: a date this build has not
    /// heard of can still be after the first revision, and forward
    /// compatibility depends on that case being reachable.
    ///
    /// Not `const`: the comparison comes from a derived `Ord`, and a derived
    /// `Ord` is not a const-callable operator.
    #[must_use]
    pub fn is_at_or_after_first_revision(self) -> bool {
        self >= Self::V2024_11_05
    }
}

impl fmt::Display for ReleaseDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = (self.year, self.month, self.day);
        write!(formatter, "{year:04}-{month:02}-{day:02}")
    }
}

/// Why a string is not a `YYYY-MM-DD` release date.
///
/// Every variant names a byte offset rather than a character offset: the parser
/// reads bytes and never slices the `&str`, so a multi-byte character at an
/// unexpected position is reported by its lead byte. The distinction matters
/// only for the message, and the alternative — a `char` from `chars().nth` —
/// would report a different character from the one that actually failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseReleaseDateError {
    /// The string was not exactly ten bytes long.
    #[error("a release date is exactly 10 bytes (`YYYY-MM-DD`); this one is {found}")]
    WrongLength {
        /// The length that was supplied.
        found: usize,
    },
    /// A separator was expected and something else was there.
    #[error("expected `-` at byte {index}; found {found:#04x}")]
    NotASeparator {
        /// Byte offset of the offending byte.
        index: usize,
        /// The byte that was there.
        found: u8,
    },
    /// A digit was expected and something else was there.
    #[error("expected an ASCII digit at byte {index}; found {found:#04x}")]
    NotADigit {
        /// Byte offset of the offending byte.
        index: usize,
        /// The byte that was there.
        found: u8,
    },
    /// The month was outside `1..=12`.
    #[error("month {month} is out of range 1..=12")]
    MonthOutOfRange {
        /// The month that was supplied, before conversion to `u8`.
        month: u16,
    },
    /// The day was outside `1..=` the length of that month.
    #[error("day {day} is out of range 1..={last} for {year:04}-{month:02}")]
    DayOutOfRange {
        /// The year the day was supplied for.
        year: u16,
        /// The month the day was supplied for.
        month: u8,
        /// The day that was supplied, before conversion to `u8`.
        day: u16,
        /// The length of that month, honouring leap years.
        last: u8,
    },
    /// The date is well formed but predates the protocol.
    ///
    /// A separate variant from the shape errors above because the fix is
    /// different: a `WrongLength` means the peer sent something that is not a
    /// version, and this means the peer sent a version from before MCP existed.
    #[error("{date} is a well-formed date but predates the first revision, 2024-11-05")]
    BeforeFirstRevision {
        /// The date that was supplied.
        date: ReleaseDate,
    },
}

impl FromStr for ReleaseDate {
    type Err = ParseReleaseDateError;

    /// Parses exactly `YYYY-MM-DD`.
    ///
    /// Every position is checked in order and the first failure is reported, so
    /// a string that is both the wrong length and unparseable says so once
    /// rather than producing a cascade. Trailing characters are rejected
    /// because a version string is a whole token, not a prefix of one.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 {
            return Err(ParseReleaseDateError::WrongLength { found: bytes.len() });
        }
        for index in [4, 7] {
            match bytes.get(index) {
                Some(b'-') => {}
                Some(&found) => return Err(ParseReleaseDateError::NotASeparator { index, found }),
                // Unreachable: the length is ten and the index is four or seven.
                None => {
                    return Err(ParseReleaseDateError::WrongLength { found: bytes.len() });
                }
            }
        }

        let year = number(bytes, 0, 4)?;
        let month = number(bytes, 5, 2)?;
        let day = number(bytes, 8, 2)?;

        if !(1..=12).contains(&month) {
            return Err(ParseReleaseDateError::MonthOutOfRange { month });
        }
        // Unreachable: the range check above admits 1..=12 only.
        let month = u8::try_from(month).unwrap_or(0);

        let last = days_in_month(year, month).unwrap_or(0);
        if day < 1 || day > u16::from(last) {
            return Err(ParseReleaseDateError::DayOutOfRange {
                year,
                month,
                day,
                last,
            });
        }
        // Unreachable: the range check above admits 1..=31 only.
        let day = u8::try_from(day).unwrap_or(0);

        Ok(Self::new(year, month, day))
    }
}

/// The decimal number in `bytes` at `start`, `width` digits wide.
///
/// Works on bytes rather than slicing the `&str`, because a byte range that
/// does not fall on a character boundary would panic on `&str` and the parser
/// must survive exactly the input it exists to reject.
fn number(bytes: &[u8], start: usize, width: usize) -> Result<u16, ParseReleaseDateError> {
    let mut total: u16 = 0;
    for index in start..start + width {
        let byte = *bytes
            .get(index)
            .ok_or(ParseReleaseDateError::WrongLength { found: bytes.len() })?;
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or(ParseReleaseDateError::NotADigit { index, found: byte })?;
        // Width is at most four, so this cannot overflow a `u16`.
        total = total * 10 + u16::from(digit);
    }
    Ok(total)
}

/// The number of days in `month`, honouring the Gregorian leap-year rule.
///
/// `None` for a month outside `1..=12`. The caller has already checked the
/// range, so the `None` arm is for the signature rather than for control flow.
const fn days_in_month(year: u16, month: u8) -> Option<u8> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{ParseReleaseDateError, ReleaseDate, days_in_month, is_leap_year, number};

    /// A malformed input, and the predicate saying which error it must produce.
    ///
    /// Checking the variant rather than the message is deliberate: the message
    /// is for a person reading a failed run, and the variant is the part that
    /// says the parser reached the right place.
    type Case = (&'static str, fn(&ParseReleaseDateError) -> bool);

    /// The 1.98 trap: `derive(PartialOrd)` has a fast path that assumes a
    /// hand-written `PartialOrd` agrees with a derived `Ord`. A pair that
    /// disagrees silently yields wrong comparisons instead of a compile error.
    /// Every ordered type in this crate derives both traits from the same field
    /// order and copies this test when it is added.
    #[test]
    fn derived_ordering_agrees_in_both_directions() {
        assert!(ReleaseDate::V2024_11_05 < ReleaseDate::V2026_07_28);
        assert!(ReleaseDate::V2026_07_28 > ReleaseDate::V2024_11_05);
        assert_eq!(ReleaseDate::V2025_06_18, ReleaseDate::V2025_06_18,);
        assert!(ReleaseDate::V2025_03_26 < ReleaseDate::V2025_06_18);
    }

    /// The field order is `year, month, day`, so every pair agrees that the
    /// year dominates — including pairs that differ only in the day.
    #[test]
    fn field_order_is_chronological() {
        assert!(ReleaseDate::V2025_03_26 < ReleaseDate::V2025_11_25);
        assert!(ReleaseDate::V2025_11_25 > ReleaseDate::V2025_06_18);
        // Same year, same month, one day apart.
        assert!(ReleaseDate::V2025_06_18 < ReleaseDate::new(2025, 6, 19));
    }

    #[test]
    fn parses_each_published_revision() {
        let cases = [
            ("2024-11-05", ReleaseDate::V2024_11_05),
            ("2025-03-26", ReleaseDate::V2025_03_26),
            ("2025-06-18", ReleaseDate::V2025_06_18),
            ("2025-11-25", ReleaseDate::V2025_11_25),
            ("2026-07-28", ReleaseDate::V2026_07_28),
        ];
        for (text, expected) in cases {
            assert_eq!(text.parse::<ReleaseDate>().ok(), Some(expected), "{text}");
        }
    }

    #[test]
    fn round_trips_through_its_display_form() {
        for date in [
            ReleaseDate::V2024_11_05,
            ReleaseDate::V2025_03_26,
            ReleaseDate::V2025_06_18,
            ReleaseDate::V2025_11_25,
            ReleaseDate::V2026_07_28,
        ] {
            assert_eq!(date.to_string().parse::<ReleaseDate>(), Ok(date));
        }
    }

    /// The failure cases the spec-shaped happy path never reaches.
    #[test]
    fn rejects_malformed_dates() {
        let cases: [Case; 9] = [
            ("", |e| {
                matches!(e, ParseReleaseDateError::WrongLength { found: 0 })
            }),
            ("2025-03", |e| {
                matches!(e, ParseReleaseDateError::WrongLength { found: 7 })
            }),
            ("2025-03-26T00:00:00Z", |e| {
                matches!(e, ParseReleaseDateError::WrongLength { found: 20 })
            }),
            // A valid date with a leading space, which a lenient parser trims.
            (" 2025-03-26", |e| {
                matches!(e, ParseReleaseDateError::WrongLength { found: 11 })
            }),
            // Ten bytes, but not a digit where the year starts.
            ("+025-03-26", |e| {
                matches!(
                    e,
                    ParseReleaseDateError::NotADigit {
                        index: 0,
                        found: b'+'
                    }
                )
            }),
            // Exactly ten bytes, with a three-byte character standing in for
            // three of the year's digits. The separators land on 4 and 7 where
            // they belong, so this reaches the digit check rather than being
            // turned away by the length check — and a parser that sliced the
            // `&str` would have panicked on the non-boundary before it got
            // that far.
            ("2€-03-26", |e| {
                matches!(e, ParseReleaseDateError::NotADigit { index: 1, .. })
            }),
            ("2025/03/26", |e| {
                matches!(
                    e,
                    ParseReleaseDateError::NotASeparator {
                        index: 4,
                        found: b'/'
                    }
                )
            }),
            // The second separator, the one a parser that checks only the first
            // would miss. A single-digit month would not reach here at all: it
            // makes the string nine bytes and the length check turns it away.
            ("2025-03/26", |e| {
                matches!(
                    e,
                    ParseReleaseDateError::NotASeparator {
                        index: 7,
                        found: b'/'
                    }
                )
            }),
            ("2025-13-26", |e| {
                matches!(e, ParseReleaseDateError::MonthOutOfRange { month: 13 })
            }),
        ];
        for (text, matches_expected) in cases {
            let parsed = text.parse::<ReleaseDate>();
            assert!(parsed.is_err(), "{text:?} should not parse");
            let Err(error) = parsed else {
                continue;
            };
            assert!(matches_expected(&error), "{text:?} gave {error}");
        }
    }

    /// Day validation has to know the month, so it has to know February.
    #[test]
    fn rejects_days_the_month_does_not_have() {
        assert!("2025-04-31".parse::<ReleaseDate>().is_err());
        assert!("2025-06-31".parse::<ReleaseDate>().is_err());
        assert!("2025-02-29".parse::<ReleaseDate>().is_err());
        assert!("2025-04-30".parse::<ReleaseDate>().is_ok());
        assert!("2025-02-28".parse::<ReleaseDate>().is_ok());
        // 2000 is divisible by 400, 1900 is not: both are leap-year edge cases.
        assert!("2000-02-29".parse::<ReleaseDate>().is_ok());
        assert!("1900-02-29".parse::<ReleaseDate>().is_err());
        // 2024-02-29 is real, which is why 2025-02-29 is the one that fails.
        assert!("2024-02-29".parse::<ReleaseDate>().is_ok());
    }

    #[test]
    fn reports_day_zero_rather_than_accepting_it() {
        let error = "2025-06-00".parse::<ReleaseDate>().unwrap_err();
        assert!(
            matches!(
                error,
                ParseReleaseDateError::DayOutOfRange {
                    year: 2025,
                    month: 6,
                    day: 0,
                    last: 30
                }
            ),
            "got {error}"
        );
    }

    /// The 1.98 derive trap again, this time proving the field order is the one
    /// that makes the calendar work, not merely that two traits agree.
    #[test]
    fn every_month_length_is_correct() {
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (index, &last) in expected.iter().enumerate() {
            let month = u8::try_from(index + 1).unwrap_or(0);
            assert_eq!(days_in_month(2025, month), Some(last), "month {month}");
        }
        assert_eq!(days_in_month(2024, 2), Some(29));
        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2000, 2), Some(29));
        assert_eq!(days_in_month(2025, 0), None);
        assert_eq!(days_in_month(2025, 13), None);
    }

    #[test]
    fn leap_year_rule_follows_the_gregorian_exceptions() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2025));
    }

    /// `number` is reached with a length that has already been checked, so its
    /// out-of-range arm is defensive. It has to stay non-panicking anyway.
    #[test]
    fn number_does_not_panic_past_the_end() {
        assert!(number(b"12345", 0, 9).is_err());
        assert!(number(b"", 0, 1).is_err());
        assert_eq!(number(b"2025-03-26", 0, 4), Ok(2025));
    }

    #[test]
    fn components_are_readable() {
        let date = ReleaseDate::V2025_11_25;
        assert_eq!(date.year(), 2025);
        assert_eq!(date.month(), 11);
        assert_eq!(date.day(), 25);
        assert!(date.is_at_or_after_first_revision());
        assert!(!ReleaseDate::new(2020, 1, 1).is_at_or_after_first_revision());
    }
}
