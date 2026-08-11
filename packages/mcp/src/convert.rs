//! Turning domain values into something a model reads correctly, and back again.
//!
//! Three jobs, and each exists because getting it wrong is silent:
//!
//! * **Money.** Sure stores signed integer minor units. A model handed `-4250` reports
//!   "$4,250" — not an error anything catches, just a wrong answer delivered confidently.
//!   Everything leaves here as a decimal string beside its currency.
//! * **Windows.** `last_90_days` and friends, resolved against a clock rather than left to
//!   a model's date arithmetic.
//! * **Tables.** A list of rows goes out as a header line plus one line each, which is
//!   roughly a quarter the tokens of the equivalent array of objects and — more usefully —
//!   reads as a table rather than as something to quote back field by field.

use chrono::{Datelike, Months, NaiveDate};
use rmcp::ErrorData;
// Taken from `rmcp` rather than declared as a dependency of our own: the derive expands to
// bare `schemars::` paths, and two crates resolving to different schemars majors would fail
// in a way whose error message never mentions the version.
use rmcp::schemars;
use rust_decimal::Decimal;

use crate::error::invalid_params;

// ---- money ---------------------------------------------------------------

/// Minor units to the decimal string a reader expects, e.g. `-4250` -> `"-42.50"`.
///
/// `decimals` is the currency's own scale (`currencies.decimal_places`) — 2 for NZD/USD,
/// 0 for JPY. Defaulting it to 2 would render ¥4250 as "¥42.50".
pub fn money_to_string(minor: i64, decimals: u32) -> String {
    Decimal::new(minor, decimals).to_string()
}

/// The inverse, for a tool that accepts an amount.
///
/// Rejects anything with more precision than the currency can hold rather than rounding it:
/// a model that writes `10.005` has made an error worth reporting, and silently storing
/// `10.00` or `10.01` would hide it inside someone's ledger.
pub fn money_from_string(s: &str, decimals: u32) -> Result<i64, ErrorData> {
    let parsed: Decimal = s
        .trim()
        .parse()
        .map_err(|_| invalid_params(format!("'{s}' is not a decimal amount, e.g. \"-42.50\"")))?;
    if parsed.scale() > decimals {
        return Err(invalid_params(format!(
            "'{s}' has more decimal places than this currency has ({decimals})"
        )));
    }
    let scaled = parsed * Decimal::from(10_i64.pow(decimals));
    scaled
        .trunc()
        .try_into()
        .map_err(|_| invalid_params(format!("'{s}' is out of range")))
}

// ---- windows -------------------------------------------------------------

/// A named date window, mirroring the SPA's global range filter.
///
/// Offered because a model asked for "the last three months" will otherwise compute the
/// dates itself, and gets it wrong often enough to matter — most reliably around month
/// ends, which for a monthly ledger is the worst place to be off by one.
///
/// The two variants with a number in them carry an explicit `rename`. `rename_all =
/// "snake_case"` does **not** put an underscore between a letter and a digit, so it renders
/// these as `last90_days` and `last12_months` — which is not what the server's own
/// instructions, `sure://conventions`, the prompts and `docs/MCP.md` all tell a caller to
/// send. Every one of those would have been rejected at deserialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    LastMonth,
    #[serde(rename = "last_90_days")]
    Last90Days,
    /// 1 January of the current year to today.
    Ytd,
    #[serde(rename = "last_12_months")]
    Last12Months,
    /// No bounds at all — every transaction on record.
    AllTime,
}

/// Every accepted `range` value, in the spelling the wire uses.
///
/// Named here so the test below can assert the enum still deserialises each one. The list is
/// repeated in prose in four places a model reads (the server instructions, the conventions
/// resource, two prompts); this is the one place a mismatch fails a build.
pub const RANGE_VALUES: [&str; 5] = [
    "last_month",
    "last_90_days",
    "ytd",
    "last_12_months",
    "all_time",
];

impl Range {
    /// `(from, to)`, both inclusive. `None` on either side means unbounded, which is what
    /// the report layer already treats as "the earliest data" / "today".
    pub fn window(self, today: NaiveDate) -> (Option<NaiveDate>, Option<NaiveDate>) {
        match self {
            // The whole of the *previous* calendar month, not "30 days ago": someone asking
            // in early March means February, all of it and nothing of March.
            Range::LastMonth => {
                let first_of_this = today.with_day(1).unwrap_or(today);
                let first_of_last = first_of_this
                    .checked_sub_months(Months::new(1))
                    .unwrap_or(first_of_this);
                let last_of_last = first_of_this.pred_opt().unwrap_or(first_of_this);
                (Some(first_of_last), Some(last_of_last))
            }
            Range::Last90Days => (today.checked_sub_days(chrono::Days::new(89)), Some(today)),
            Range::Ytd => (NaiveDate::from_ymd_opt(today.year(), 1, 1), Some(today)),
            Range::Last12Months => (today.checked_sub_months(Months::new(12)), Some(today)),
            Range::AllTime => (None, None),
        }
    }
}

/// Resolve the window a tool was asked for.
///
/// Explicit `from`/`to` win over `range`, so a caller can narrow one edge of a named window
/// without having to compute both. Given neither, the report layer's own defaults apply.
pub fn resolve_window(
    range: Option<Range>,
    from: Option<String>,
    to: Option<String>,
    today: NaiveDate,
) -> Result<(Option<String>, Option<String>), ErrorData> {
    let (range_from, range_to) = match range {
        Some(r) => r.window(today),
        None => (None, None),
    };
    let parse = |s: &str| -> Result<String, ErrorData> {
        NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map(|d| d.to_string())
            .map_err(|_| invalid_params(format!("'{s}' is not an ISO-8601 date (YYYY-MM-DD)")))
    };
    let from = match from.as_deref() {
        Some(s) => Some(parse(s)?),
        None => range_from.map(|d| d.to_string()),
    };
    let to = match to.as_deref() {
        Some(s) => Some(parse(s)?),
        None => range_to.map(|d| d.to_string()),
    };
    if let (Some(f), Some(t)) = (&from, &to)
        && f > t
    {
        return Err(invalid_params(format!(
            "the window starts after it ends: from={f}, to={t}"
        )));
    }
    Ok((from, to))
}

// ---- tables --------------------------------------------------------------

/// A pipe-delimited table: a header line, then one line per row.
///
/// Not markdown — no alignment padding and no `---` rule, both of which cost tokens to say
/// nothing. A cell containing `|` would break the shape, so it is replaced rather than
/// escaped: this is a display format, and the id in the first column is what a follow-up
/// call actually uses.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::with_capacity(rows.len() * 64);
    out.push_str(&headers.join(" | "));
    for row in rows {
        out.push('\n');
        out.push_str(
            &row.iter()
                .map(|c| c.replace('|', "/").replace('\n', " "))
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    out
}

/// The note a capped list ends with.
///
/// Says the cap was hit *and* what to do instead. A bare "showing 50 of many" invites the
/// model to page through the rest one call at a time, which is the traffic the cap exists
/// to prevent.
pub fn truncation_note(shown: usize, next_offset: i64) -> String {
    format!(
        "\n\n(showing {shown}; more rows matched. Narrow the filter, pass offset={next_offset} \
         for the next page, or use summarize_spending if you want totals rather than rows.)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The bug this test exists for: `rename_all = "snake_case"` renders `Last90Days` as
    /// `last90_days`, so every `range` the server's own instructions tell a caller to send
    /// was refused at deserialisation. Nothing else noticed, because the tests that used a
    /// window passed explicit `from`/`to` instead.
    #[test]
    fn every_documented_range_value_actually_deserialises() {
        for value in RANGE_VALUES {
            let parsed: Result<Range, _> = serde_json::from_value(serde_json::json!(value));
            assert!(
                parsed.is_ok(),
                "`{value}` is offered to callers but is not a value this enum accepts"
            );
        }
    }

    /// And the reverse: nothing accepts a spelling that is not offered, so the documented
    /// list stays the whole list.
    #[test]
    fn the_documented_list_is_the_complete_list() {
        assert_eq!(
            RANGE_VALUES.len(),
            5,
            "a range was added without documenting it"
        );
        for wrong in ["last90_days", "last12_months", "last_3_months", "yesterday"] {
            let parsed: Result<Range, _> = serde_json::from_value(serde_json::json!(wrong));
            assert!(parsed.is_err(), "`{wrong}` should not be accepted");
        }
    }

    /// The whole reason this module exists.
    #[test]
    fn money_leaves_as_a_decimal_not_as_minor_units() {
        assert_eq!(money_to_string(-4250, 2), "-42.50");
        assert_eq!(money_to_string(0, 2), "0.00");
        assert_eq!(money_to_string(114_269_63, 2), "114269.63");
        // A zero-decimal currency is not a two-decimal one with the point moved.
        assert_eq!(money_to_string(4250, 0), "4250");
    }

    #[test]
    fn an_amount_round_trips_through_its_wire_form() {
        for minor in [-4250_i64, 0, 1, 114_269_63, -1] {
            let s = money_to_string(minor, 2);
            assert_eq!(money_from_string(&s, 2).unwrap(), minor, "{s}");
        }
    }

    #[test]
    fn an_over_precise_amount_is_refused_rather_than_rounded() {
        let err = money_from_string("10.005", 2).unwrap_err();
        assert!(err.message.contains("decimal places"), "{}", err.message);
        // The same digits are fine where the currency has the scale for them.
        assert_eq!(money_from_string("10.005", 3).unwrap(), 10_005);
    }

    #[test]
    fn a_non_numeric_amount_says_what_one_looks_like() {
        let err = money_from_string("$42.50", 2).unwrap_err();
        assert!(err.message.contains("-42.50"), "{}", err.message);
    }

    /// "Last month" is the previous *calendar* month — asked on 3 March, February.
    #[test]
    fn last_month_is_the_previous_calendar_month_not_thirty_days() {
        let (from, to) = Range::LastMonth.window(d("2026-03-03"));
        assert_eq!(from, Some(d("2026-02-01")));
        assert_eq!(to, Some(d("2026-02-28")));
    }

    /// Asked on 1 January, "last month" is the whole of the previous December — the case a
    /// month-arithmetic slip turns into an empty report.
    #[test]
    fn last_month_crosses_the_year_boundary() {
        let (from, to) = Range::LastMonth.window(d("2026-01-01"));
        assert_eq!(from, Some(d("2025-12-01")));
        assert_eq!(to, Some(d("2025-12-31")));
    }

    #[test]
    fn the_named_windows_are_inclusive_of_today() {
        // 90 days *including* today, so the span is 89 days back.
        assert_eq!(
            Range::Last90Days.window(d("2026-08-10")),
            (Some(d("2026-05-13")), Some(d("2026-08-10")))
        );
        assert_eq!(
            Range::Ytd.window(d("2026-08-10")),
            (Some(d("2026-01-01")), Some(d("2026-08-10")))
        );
        assert_eq!(Range::AllTime.window(d("2026-08-10")), (None, None));
    }

    #[test]
    fn an_explicit_bound_overrides_the_named_range_on_that_side_only() {
        let (from, to) = resolve_window(
            Some(Range::Ytd),
            Some("2026-06-01".to_string()),
            None,
            d("2026-08-10"),
        )
        .unwrap();
        assert_eq!(from.as_deref(), Some("2026-06-01"));
        assert_eq!(
            to.as_deref(),
            Some("2026-08-10"),
            "the range still supplies the other edge"
        );
    }

    #[test]
    fn a_malformed_date_is_refused_with_the_shape_it_wanted() {
        let err =
            resolve_window(None, Some("June 2026".into()), None, d("2026-08-10")).unwrap_err();
        assert!(err.message.contains("YYYY-MM-DD"), "{}", err.message);
    }

    /// A backwards window silently returns nothing, which reads as "you spent nothing".
    #[test]
    fn a_backwards_window_is_an_error_not_an_empty_report() {
        let err = resolve_window(
            None,
            Some("2026-08-01".into()),
            Some("2026-07-01".into()),
            d("2026-08-10"),
        )
        .unwrap_err();
        assert!(
            err.message.contains("starts after it ends"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_table_is_a_header_and_one_line_per_row() {
        let out = table(
            &["id", "amount"],
            &[
                vec!["1".into(), "-42.50".into()],
                vec!["2".into(), "10.00".into()],
            ],
        );
        assert_eq!(out, "id | amount\n1 | -42.50\n2 | 10.00");
    }

    /// A payee containing a pipe or a newline must not be able to forge a column or a row.
    #[test]
    fn a_cell_cannot_break_out_of_its_column_or_its_row() {
        let out = table(
            &["id", "description"],
            &[vec!["1".into(), "ACME | LTD\nsecond line".into()]],
        );
        assert_eq!(out, "id | description\n1 | ACME / LTD second line");
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn the_truncation_note_points_at_the_aggregate_not_just_the_next_page() {
        let note = truncation_note(50, 50);
        assert!(note.contains("showing 50"), "{note}");
        assert!(note.contains("offset=50"), "{note}");
        assert!(note.contains("summarize_spending"), "{note}");
    }
}
