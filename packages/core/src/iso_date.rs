//! [`IsoDate`] — a calendar date that has actually been parsed.
//!
//! Every date this system stores is a bare `TEXT` column whose only contract is a comment
//! (`posted_at TEXT NOT NULL, -- ISO-8601`), and every reader — the reports, the forecast,
//! SQLite's own `date()` in the list filter — reads it back with `%Y-%m-%d`. A value in any
//! other shape therefore doesn't fail anywhere: it inserts fine, comes back from
//! `GET /api/transactions` fine, renders in the ledger fine, and is then *invisible* to the
//! balance sheet, net worth, category breakdown, money-flow graph, equity position and
//! forecast, because each of those quietly drops a row whose date won't parse. The
//! transaction list and the balance disagree permanently with no error anywhere, and
//! `?from=`/`?to=` hides the row too (`date('31/07/2026')` is NULL), which is exactly how
//! such a row evades discovery.
//!
//! So the date is parsed once, at the wire edge, into this type (CLAUDE.md rule 1) — and
//! never re-guessed downstream. A bad date is a 422 at the body extractor, before any
//! statement is built, instead of a 201 followed by a permanent silent discrepancy.

use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{AppError, AppResult};

/// The exact width of the one accepted form, `YYYY-MM-DD`.
const ISO_DATE_LEN: usize = 10;

/// Earliest year accepted. Nothing in a personal ledger predates it, and a date far outside
/// the window is bad data (a mis-parsed field, a garbled import) rather than history.
pub const MIN_YEAR: i32 = 1900;

/// Latest year accepted. The report date-sampling is point-capped, so a year-1000 or
/// year-9999 row cannot hang anything — but it stretches every chart's x-axis over a
/// millennium and makes the useful part of the series an unreadable smear, which is its own
/// kind of silent data loss.
pub const MAX_YEAR: i32 = 2199;

/// A calendar date in `YYYY-MM-DD` form, validated on construction.
///
/// The inner [`NaiveDate`] is private and the only constructors check: there is no path that
/// produces an `IsoDate` holding something a report can't read. Because the accepted input is
/// exactly ten characters with a four-digit year, [`Serialize`] (via [`fmt::Display`])
/// reproduces the caller's bytes exactly — the wire contract is unchanged, the field is still
/// a JSON string, and the generated OpenAPI/TS client still says `string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IsoDate(NaiveDate);

impl IsoDate {
    /// The one fallible constructor. Rejects, in order: anything that isn't exactly
    /// `YYYY-MM-DD` (so `31/07/2026`, `2026-7-1`, `""`, a full RFC-3339 datetime and
    /// trailing junk are all out — note `chrono` alone accepts `2026-7-1` for `%Y-%m-%d`,
    /// and a zero-padded shape is what keeps SQLite's *lexicographic* `as_of <= ?1`
    /// comparisons — which is how every balance query bounds a date — honest), then a
    /// non-existent calendar date (`2026-02-30`), then a year outside [`MIN_YEAR`]..=[`MAX_YEAR`].
    ///
    /// Surrounding whitespace is trimmed first, matching what the DAL used to do at every
    /// bind site; that trimming now happens once, here.
    pub fn parse(s: &str) -> AppResult<Self> {
        Self::from_wire(s).map_err(AppError::validation)
    }

    /// The check itself, with a plain-`String` error so [`Deserialize`] can hand it to
    /// `serde::de::Error::custom` and the HTTP layer can turn it into a field-named 422.
    fn from_wire(s: &str) -> Result<Self, String> {
        let t = s.trim();
        if !is_iso_shape(t) {
            return Err(format!(
                "invalid date {t:?}: expected an ISO-8601 calendar date in YYYY-MM-DD form, \
                 zero-padded (e.g. 2026-07-31)"
            ));
        }
        let date = NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .map_err(|_| format!("invalid date {t:?}: no such day in the calendar"))?;
        if !(MIN_YEAR..=MAX_YEAR).contains(&date.year()) {
            return Err(format!(
                "date {t:?} is outside the supported range {MIN_YEAR}-01-01..={MAX_YEAR}-12-31"
            ));
        }
        Ok(Self(date))
    }

    /// Promote an already-structurally-valid [`NaiveDate`], still applying the plausibility
    /// window — a date computed from a bad offset is as wrong as one that was typed.
    pub fn from_date(date: NaiveDate) -> AppResult<Self> {
        Self::parse(&date.format("%Y-%m-%d").to_string())
    }

    /// The parsed date, for arithmetic and comparison. Free: no re-parsing, because the
    /// value was parsed once on the way in.
    pub fn date(self) -> NaiveDate {
        self.0
    }
}

/// Whether `s` is *syntactically* `YYYY-MM-DD`: ten bytes, ASCII digits everywhere except
/// dashes at index 4 and 7. Deliberately stricter than `chrono`'s `%Y-%m-%d`, which happily
/// accepts `2026-7-1`.
fn is_iso_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == ISO_DATE_LEN
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

impl fmt::Display for IsoDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `%Y` zero-pads to four digits, and the year is bounded to 1900..=2199, so this is
        // byte-for-byte what was accepted.
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

impl FromStr for IsoDate {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<NaiveDate> for IsoDate {
    type Error = AppError;

    fn try_from(date: NaiveDate) -> Result<Self, Self::Error> {
        Self::from_date(date)
    }
}

impl From<IsoDate> for NaiveDate {
    fn from(d: IsoDate) -> Self {
        d.0
    }
}

/// Parses as a JSON string and then applies the check, so a bad date is refused by the body
/// extractor (422, naming the field) before a handler — let alone a statement — sees it.
impl<'de> Deserialize<'de> for IsoDate {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(de)?;
        Self::from_wire(&raw).map_err(serde::de::Error::custom)
    }
}

/// Renders the same ten characters it accepted, so putting the type on a response DTO can
/// never change a wire body.
impl Serialize for IsoDate {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps the value the way every wire DTO does, so these exercise the real
    /// serde path rather than `IsoDate::parse` directly.
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Wrap {
        posted_at: IsoDate,
    }

    fn de(raw: &str) -> Result<Wrap, serde_json::Error> {
        serde_json::from_str(&format!(r#"{{"posted_at":{raw}}}"#))
    }

    #[test]
    fn accepts_a_zero_padded_iso_date() {
        let w = de(r#""2026-07-31""#).unwrap();
        assert_eq!(
            w.posted_at.date(),
            NaiveDate::from_ymd_opt(2026, 7, 31).unwrap()
        );
    }

    /// The exact bug this type exists for: a day-first date is a 422, not a stored row that
    /// no report can see.
    #[test]
    fn rejects_day_first() {
        let err = de(r#""31/07/2026""#).unwrap_err().to_string();
        assert!(err.contains("YYYY-MM-DD"), "unhelpful message: {err}");
        assert!(
            err.contains("31/07/2026"),
            "message should quote the value: {err}"
        );
    }

    /// `chrono`'s `%Y-%m-%d` accepts this on its own; we must not, because SQLite compares
    /// these columns as text and `"2026-7-1" > "2026-12-01"` lexicographically.
    #[test]
    fn rejects_unpadded_components() {
        assert!(de(r#""2026-7-1""#).is_err());
        assert!(de(r#""26-07-01""#).is_err());
    }

    #[test]
    fn rejects_empty_and_garbage() {
        assert!(de(r#""""#).is_err());
        assert!(de(r#""not a date""#).is_err());
        assert!(de(r#""          ""#).is_err());
    }

    /// A datetime is the shape a provider payload carries (`2026-01-06T09:30:00+00:00`);
    /// truncating it silently would store a date the user never sent.
    #[test]
    fn rejects_a_datetime() {
        assert!(de(r#""2026-07-31T09:30:00Z""#).is_err());
        assert!(de(r#""2026-07-31 09:30:00""#).is_err());
        assert!(de(r#""2026-07-31extra""#).is_err());
    }

    #[test]
    fn rejects_a_nonexistent_calendar_day() {
        let err = de(r#""2026-02-30""#).unwrap_err().to_string();
        assert!(err.contains("no such day"), "{err}");
        assert!(de(r#""2026-13-01""#).is_err());
        assert!(de(r#""2026-00-10""#).is_err());
    }

    #[test]
    fn rejects_years_outside_the_plausible_window() {
        let err = de(r#""1000-01-01""#).unwrap_err().to_string();
        assert!(err.contains("outside the supported range"), "{err}");
        assert!(de(r#""9999-12-31""#).is_err());
        assert!(de(r#""0001-01-01""#).is_err());
        // The boundaries themselves are in.
        assert!(de(r#""1900-01-01""#).is_ok());
        assert!(de(r#""2199-12-31""#).is_ok());
    }

    #[test]
    fn rejects_a_non_string() {
        assert!(de("20260731").is_err());
        assert!(de("null").is_err());
    }

    /// The wire contract is unchanged: what came in is what goes out, byte for byte.
    #[test]
    fn round_trips_byte_identically() {
        for raw in ["2026-07-31", "1900-01-01", "2199-12-31", "2024-02-29"] {
            let json = format!(r#"{{"posted_at":"{raw}"}}"#);
            let w: Wrap = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&w).unwrap(), json);
            assert_eq!(w.posted_at.to_string(), raw);
        }
    }

    /// Whitespace is absorbed once, here, rather than at every DAL bind site.
    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            de(r#"" 2026-07-31 ""#).unwrap().posted_at.to_string(),
            "2026-07-31"
        );
    }

    #[test]
    fn parse_and_from_date_agree() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 31).unwrap();
        assert_eq!(
            IsoDate::from_date(d).unwrap(),
            IsoDate::parse("2026-07-31").unwrap()
        );
        assert!(IsoDate::from_date(NaiveDate::from_ymd_opt(1000, 1, 1).unwrap()).is_err());
        assert!("2026-07-31".parse::<IsoDate>().is_ok());
        assert!("31/07/2026".parse::<IsoDate>().is_err());
    }

    /// Ordering follows the calendar, so a `Vec<IsoDate>` sorts the way a report needs.
    #[test]
    fn orders_chronologically() {
        let mut v = [
            IsoDate::parse("2026-12-01").unwrap(),
            IsoDate::parse("2026-07-31").unwrap(),
        ];
        v.sort();
        assert_eq!(v[0].to_string(), "2026-07-31");
    }
}
