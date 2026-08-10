use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::error::{AppError, AppResult};
use crate::iso_date::IsoDate;
use crate::money::Money;
use crate::people::Ownership;

/// The most ids one bulk mutation may carry.
///
/// Each id becomes its own bound variable in the `WHERE id IN (…)` list the DAL builds
/// (`sure_dal::transactions::bulk_update` / `bulk_delete`), and SQLite refuses to *prepare* a
/// statement holding more than `SQLITE_MAX_VARIABLE_NUMBER` binds — 32766 by default. Roughly
/// 130k ids still fit inside the 2 MiB request-body ceiling, so a select-all over a real
/// ledger reaches that limit without anyone attacking anything; the failure arrives as a
/// prepare error, which the DAL's `map_fk` catch-all folds into `AppError::Database` and the
/// HTTP layer scrubs to a bare 500 "Internal Error" — no hint that a smaller batch would
/// work. Capping here turns that into a 422 naming the limit.
///
/// The value has a floor and a ceiling, and 5000 sits between them with room to spare:
///
/// * it must stay *above* the SPA's transaction page size (`limit: 2000` in
///   `Transactions.svelte`), because "select all" sends every loaded row's id in one request —
///   a cap under that would turn a normal click into a 422;
/// * it must stay well *under* 32766, with slack for the other binds a statement carries (the
///   `SET` clause contributes a handful) and for that page size growing again later.
pub const MAX_BULK_IDS: usize = 5000;

/// A bulk mutation's id list, checked as it is parsed: never empty, never longer than
/// [`MAX_BULK_IDS`].
///
/// The bound lives on the type, not in each handler (CLAUDE.md rule 1), so every caller —
/// the HTTP body, a future CLI, a test — is refused identically, and the DAL may build its
/// `IN (…)` list knowing the bind count is already bounded. The inner `Vec` is private: the
/// only way to obtain a `BulkIds` is [`BulkIds::new`], so there is no path that skips the
/// check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkIds(Vec<i64>);

impl BulkIds {
    /// The one constructor. The error is an `AppError::Validation` (422) whose message names
    /// the limit *and* the offending count, so a client that hit it knows what batch size to
    /// retry with instead of guessing.
    pub fn new(ids: Vec<i64>) -> AppResult<Self> {
        let count = ids.len();
        if count == 0 {
            return Err(AppError::validation(format!(
                "ids must not be empty (1 to {MAX_BULK_IDS} ids per bulk request)"
            )));
        }
        if count > MAX_BULK_IDS {
            return Err(AppError::validation(format!(
                "too many ids: {count} (maximum {MAX_BULK_IDS} per bulk request — split the selection into smaller batches)"
            )));
        }
        Ok(Self(ids))
    }

    pub fn as_slice(&self) -> &[i64] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<i64> {
        self.0
    }
}

/// Lets a `&BulkIds` stand in for the `&[i64]` the DAL and the repository ports take, so the
/// cap costs the call sites nothing.
impl std::ops::Deref for BulkIds {
    type Target = [i64];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<Vec<i64>> for BulkIds {
    type Error = AppError;

    fn try_from(ids: Vec<i64>) -> Result<Self, Self::Error> {
        Self::new(ids)
    }
}

/// Parses as a plain JSON array of integers and then applies the bound, so an over-sized
/// batch is rejected by the body extractor (422) before a statement is ever built.
impl<'de> Deserialize<'de> for BulkIds {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ids = Vec::<i64>::deserialize(de)?;
        Self::new(ids).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct Transaction {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    /// Signed minor units in `currency_code`; negative = outflow.
    pub amount_minor: i64,
    pub currency_code: String,
    pub description: String,
    /// Raw merchant text (e.g. from an import).
    pub merchant: Option<String>,
    /// Resolved custom merchant, if assigned.
    pub merchant_id: Option<i64>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    /// Excluded from regular reports when true.
    pub is_one_off: bool,
    /// The other side of a transfer, if linked.
    pub linked_transaction_id: Option<i64>,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    /// Which rule (if any) last set this transaction's category.
    pub categorized_by_rule_id: Option<i64>,
    /// Attribution *override*: who this one transaction belongs to, when that isn't simply
    /// its account's owner. `None` — the usual case, and what every import produces — means
    /// it follows the account (see [`crate::effective_ownership`]).
    pub ownership: Option<Ownership>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveTransaction {
    pub account_id: i64,
    #[schema(value_type = String)]
    pub posted_at: IsoDate,
    #[schema(value_type = i64)]
    pub amount_minor: Money,
    /// Defaults to the account's currency when omitted.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub merchant: Option<String>,
    #[serde(default)]
    pub merchant_id: Option<i64>,
    /// Attribution override; omit (or send `null`) to follow the account's owner.
    #[serde(default)]
    pub ownership: Option<Ownership>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub is_one_off: bool,
}

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxQuery {
    pub account_id: Option<i64>,
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, one-off transactions are excluded. Defaults to true.
    pub include_one_off: Option<bool>,
    /// Case-insensitive substring match on description/merchant/notes.
    pub search: Option<String>,
    /// `true` keeps only rows with no category, `false` only rows that have one; omitted
    /// means both. Distinct from `category_id`, which can only ever name a category that
    /// exists — there is no id for "none", so the gap this closes could not be expressed.
    pub uncategorized: Option<bool>,
    /// Restrict to transactions whose *effective* attribution (override, else the account's
    /// owner) is this. Parsed from `?attributed_to=joint|<person id>` at the HTTP edge.
    pub attributed_to: Option<Ownership>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkRequest {
    pub linked_transaction_id: i64,
}

/// A partial patch applied to every transaction in `ids` at once. Each optional field
/// that is *present* is written to all of them; absent fields are left untouched. The
/// nullable id fields use a nested option so a JSON `null` (clear the value) is distinct
/// from an omitted field (leave as-is) — the same distinction the inline edits rely on.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkUpdate {
    /// The transactions to patch: 1 to 5000 ids. An empty or over-long list is refused by the
    /// body extractor before any statement is built.
    ///
    /// The bound is [`MAX_BULK_IDS`]; the literal below has to stay in step with it, because
    /// utoipa's `max_items` only accepts a number literal, and
    /// `bulk_ids_cap_matches_the_documented_schema_bound` fails if the two drift apart.
    #[schema(value_type = Vec<i64>, min_items = 1, max_items = 5000)]
    pub ids: BulkIds,
    /// Present → set the category (or clear it with `null`); absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub category_id: Option<Option<i64>>,
    /// Present → set the merchant (or clear it with `null`); absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub merchant_id: Option<Option<i64>>,
    /// Present → set the one-off flag; absent → leave unchanged.
    #[serde(default)]
    pub is_one_off: Option<bool>,
    /// Present → override the attribution (or `null` to go back to following the account);
    /// absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub ownership: Option<Option<Ownership>>,
}

/// The ids to delete in a single bulk request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkDelete {
    /// The transactions to delete: 1 to 5000 ids. An empty or over-long list is refused by the
    /// body extractor before any statement is built.
    ///
    /// The bound is [`MAX_BULK_IDS`]; see the note on [`BulkUpdate::ids`] about keeping that
    /// constant and the literal below in step.
    #[schema(value_type = Vec<i64>, min_items = 1, max_items = 5000)]
    pub ids: BulkIds,
}

/// Result of a bulk mutation: how many transactions were affected.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkResult {
    pub affected: i64,
}

/// Deserialize into `Option<Option<T>>` such that a present `null` becomes `Some(None)`
/// (an explicit clear) while an omitted field — via `#[serde(default)]` — stays `None`
/// (leave unchanged). Plain `Option<Option<T>>` can't tell the two apart on its own.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TransferRequest {
    pub from_account_id: i64,
    pub to_account_id: i64,
    #[schema(value_type = String)]
    pub posted_at: IsoDate,
    /// Amount leaving the source account (positive minor units).
    // `Money` rather than `i64` for a reason beyond the shared ceiling: the writer normalises
    // the direction with `.abs()`, and on a raw `i64` the single input `i64::MIN` panics in
    // debug and returns `i64::MIN` in release — which the outflow leg then negates again. See
    // `Money::abs`. Not a doc comment, deliberately: utoipa would put it in the OpenAPI
    // `description`, which regenerates `packages/client/src/schema.d.ts` for no wire change.
    #[schema(value_type = i64)]
    pub from_amount_minor: Money,
    /// Amount arriving in the destination account; defaults to `from_amount_minor`
    /// (set explicitly for cross-currency transfers).
    #[serde(default)]
    #[schema(value_type = Option<i64>)]
    pub to_amount_minor: Option<Money>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<i64> {
        (1..=n as i64).collect()
    }

    fn bulk_update_json(ids: &[i64]) -> String {
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        format!(r#"{{"ids":[{list}],"is_one_off":true}}"#)
    }

    #[test]
    fn an_ordinary_batch_is_accepted() {
        let parsed: BulkUpdate = serde_json::from_str(&bulk_update_json(&[7, 9])).unwrap();
        assert_eq!(parsed.ids.as_slice(), &[7, 9]);
        assert_eq!(parsed.is_one_off, Some(true));

        let parsed: BulkDelete = serde_json::from_str(r#"{"ids":[7,9]}"#).unwrap();
        // The `Deref` is what lets the DAL and the repository port keep taking `&[i64]`: this
        // is the exact shape of the `bulk_delete(&input.ids)` call in `sure_api`, and it has to
        // keep compiling without a change there.
        fn port_signature(ids: &[i64]) -> usize {
            ids.len()
        }
        assert_eq!(port_signature(&parsed.ids), 2);
        assert_eq!(&*parsed.ids, &[7, 9]);
    }

    #[test]
    fn a_batch_at_exactly_the_cap_is_accepted() {
        let at_cap = ids(MAX_BULK_IDS);
        assert_eq!(BulkIds::new(at_cap.clone()).unwrap().len(), MAX_BULK_IDS);

        let parsed: BulkDelete = serde_json::from_str(&format!(
            r#"{{"ids":[{}]}}"#,
            at_cap
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ))
        .unwrap();
        assert_eq!(parsed.ids.len(), MAX_BULK_IDS);
    }

    #[test]
    fn a_batch_over_the_cap_is_a_validation_error_naming_the_limit() {
        let over = ids(MAX_BULK_IDS + 1);
        let err = BulkIds::new(over.clone()).unwrap_err();
        assert_eq!(err.code(), "validation", "must be a 422, not a 500");
        let message = err.to_string();
        assert!(
            message.contains(&MAX_BULK_IDS.to_string()),
            "message must name the limit so a client can batch: {message}"
        );
        assert!(
            message.contains(&over.len().to_string()),
            "message must name the offending count: {message}"
        );

        // …and the same through the wire form, which is where it actually bites: the body
        // extractor refuses it before any statement is prepared, so SQLite's bind-variable
        // limit is never reached and the client gets a message instead of a scrubbed 500.
        let err = serde_json::from_str::<BulkUpdate>(&bulk_update_json(&over)).unwrap_err();
        assert!(
            err.to_string().contains(&MAX_BULK_IDS.to_string()),
            "deserialize error must carry the limit through: {err}"
        );
    }

    #[test]
    fn an_empty_batch_is_rejected() {
        let err = BulkIds::new(vec![]).unwrap_err();
        assert_eq!(err.code(), "validation");
        assert!(
            err.to_string().contains("empty"),
            "message should say the list was empty: {err}"
        );

        // A caller sending `{"ids":[]}` meant to select something; answering "0 affected"
        // hides a bug in whatever built the selection.
        assert!(serde_json::from_str::<BulkDelete>(r#"{"ids":[]}"#).is_err());
        assert!(serde_json::from_str::<BulkUpdate>(&bulk_update_json(&[])).is_err());
    }

    #[test]
    fn bulk_ids_cap_matches_the_documented_schema_bound() {
        // utoipa's `max_items` only takes a literal, so the OpenAPI bound on `BulkUpdate::ids`
        // and `BulkDelete::ids` is hand-written. If this constant moves, those two literals
        // (and the generated client) have to move with it.
        assert_eq!(MAX_BULK_IDS, 5000);
        // The floor the SPA's select-all needs: a page of `limit: 2000` rows must fit in one
        // request (see the constant's docs).
        const { assert!(MAX_BULK_IDS >= 2000) };
    }

    /// The reported payload, at the extractor: `i64::MAX` was a 201 twice over, and the
    /// balance walk then added the two rows together. It has to be a 422 before any statement
    /// is built — the row must never reach the column, because from there the only options are
    /// a 500 on every report or a wrong number.
    #[test]
    fn a_transaction_amount_past_the_ceiling_is_refused_at_the_extractor() {
        let body = |amount: &str| {
            format!(
                r#"{{"account_id":1,"posted_at":"2026-07-31","amount_minor":{amount},"description":"x"}}"#
            )
        };
        for amount in [
            "9223372036854775807",
            "-9223372036854775808",
            "100000000000001",
        ] {
            let err = serde_json::from_str::<SaveTransaction>(&body(amount))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("out of range"),
                "{amount} must be refused with a message that says why: {err}"
            );
        }
        // Ordinary and at-the-ceiling amounts still parse, and keep their sign.
        let ok: SaveTransaction = serde_json::from_str(&body("-4250")).unwrap();
        assert_eq!(ok.amount_minor.minor(), -4250);
        let at_cap: SaveTransaction =
            serde_json::from_str(&body(&crate::MAX_MONEY_MINOR.to_string())).unwrap();
        assert_eq!(at_cap.amount_minor.minor(), crate::MAX_MONEY_MINOR);
    }

    /// `i64::MIN` through the transfer route, which is the sharper half of the bug: the DAL
    /// normalises the direction with `.abs()`, and `i64::MIN.abs()` panics in debug and stays
    /// `i64::MIN` in release — which the outflow bind then negates *again*. Refusing the value
    /// at the extractor is what makes `Money::abs` total downstream.
    #[test]
    fn a_transfer_amount_past_the_ceiling_is_refused_at_the_extractor() {
        let body = |from: &str, to: &str| {
            format!(
                r#"{{"from_account_id":1,"to_account_id":2,"posted_at":"2026-07-31",
                     "from_amount_minor":{from},"to_amount_minor":{to}}}"#
            )
        };
        assert!(
            serde_json::from_str::<TransferRequest>(&body("-9223372036854775808", "null"))
                .unwrap_err()
                .to_string()
                .contains("out of range")
        );
        // The destination leg is bounded too — a cross-currency transfer sets it explicitly.
        assert!(
            serde_json::from_str::<TransferRequest>(&body("100", "9223372036854775807")).is_err()
        );

        let ok: TransferRequest = serde_json::from_str(&body("25000", "null")).unwrap();
        assert_eq!(ok.from_amount_minor.minor(), 250_00);
        assert_eq!(
            ok.to_amount_minor, None,
            "an omitted leg mirrors the source"
        );
        // …and the normalisation the DAL performs is total on the parsed value.
        assert_eq!(crate::Money::new(-250_00).unwrap().abs().minor(), 250_00);
    }

    #[test]
    fn try_from_is_the_same_check() {
        assert!(BulkIds::try_from(ids(2)).is_ok());
        assert!(BulkIds::try_from(ids(MAX_BULK_IDS + 1)).is_err());
        assert_eq!(BulkIds::new(ids(3)).unwrap().into_inner(), vec![1, 2, 3]);
    }
}
