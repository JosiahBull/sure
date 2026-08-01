//! Household individuals, and how a thing is attributed to one of them.
//!
//! Sure is single-login by design — there is no auth, and a person here is a *label on the
//! money*, not a user account. The household is whoever shares the finances being tracked.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One member of the household.
#[derive(Debug, Serialize, ToSchema, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    /// Badge/chart colour, a CSS hex string (`#rrggbb`). `None` lets the web layer derive
    /// one from the id, the way it already does for categories.
    pub color: Option<String>,
    pub sort_order: i64,
    /// True for the stand-in the household-required migration created to own accounts that
    /// predate the feature. Not something a caller sets: it's the app admitting it doesn't
    /// know whose those accounts are, and it clears itself the moment this person is
    /// renamed (an explicit name is the answer it was standing in for).
    pub placeholder: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SavePerson {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

/// Who an account belongs to. Every account has one — there is no unattributed state.
///
/// Stored as the `(accounts.ownership, accounts.person_id)` column pair and parsed into
/// this enum the moment a row is read — the columns are the serialised edge, this is the
/// value everything above the DAL passes around (CLAUDE.md rule 1). A pair of database
/// triggers enforces the same two shapes, so a row cannot exist in any other state however
/// it was written.
///
/// Accounts that predate the household feature are owned by the *placeholder* person the
/// household-required migration created (see [`Person::placeholder`]) rather than by a
/// guess at a real one — the requirement is satisfied without inventing an answer.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ownership {
    /// Belongs to one individual.
    Person { person_id: i64 },
    /// Shared by the household — a joint bank account, the family home.
    Joint,
}

impl Ownership {
    /// The stored `ownership` discriminant (matches
    /// `#[serde(tag = "kind", rename_all = "snake_case")]`).
    pub fn as_str(self) -> &'static str {
        match self {
            Ownership::Person { .. } => "person",
            Ownership::Joint => "joint",
        }
    }

    /// The person this belongs to, if it belongs to exactly one.
    pub fn person_id(self) -> Option<i64> {
        match self {
            Ownership::Person { person_id } => Some(person_id),
            Ownership::Joint => None,
        }
    }

    /// Split into the two columns to bind, in `(ownership, person_id)` order. The only
    /// place this value becomes text.
    pub fn as_parts(self) -> (&'static str, Option<i64>) {
        (self.as_str(), self.person_id())
    }

    /// Rebuild from the two stored columns. Both halves have to agree — a `'person'` row
    /// with no `person_id` (or a `'joint'` row carrying one) is a row written by something
    /// that went around every writer we own, and is reported as such rather than being
    /// coerced into whichever half looks more plausible.
    pub fn from_stored(ownership: &str, person_id: Option<i64>) -> Result<Self, String> {
        match (ownership, person_id) {
            ("person", Some(person_id)) => Ok(Ownership::Person { person_id }),
            ("person", None) => Err("account ownership 'person' has no person_id".to_string()),
            ("joint", None) => Ok(Ownership::Joint),
            ("joint", Some(_)) => {
                Err("account ownership 'joint' must not carry a person_id".to_string())
            }
            (other, _) => Err(format!("unknown account ownership '{other}'")),
        }
    }
}

impl std::str::FromStr for Ownership {
    type Err = String;

    /// Parse the query-string form used by `?attributed_to=` — `joint`, or a person's id.
    /// The HTTP edge is the one place this value is text (CLAUDE.md rule 1); an
    /// unrecognised value is a 400 rather than a silently ignored filter, which would
    /// otherwise answer "whose spending is this?" with everyone's.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "joint" {
            return Ok(Ownership::Joint);
        }
        s.parse::<i64>()
            .map(|person_id| Ownership::Person { person_id })
            .map_err(|_| format!("unknown attribution '{s}' (expected 'joint' or a person id)"))
    }
}

/// Who a transaction belongs to: its own override if it has one, otherwise its account's
/// owner.
///
/// The one place this rule lives. A transaction with no override follows its account
/// forever — including retroactively, so re-attributing an account moves its whole history
/// with it, which is exactly what you want the first time you sort out whose accounts are
/// whose.
pub fn effective_ownership(transaction: Option<Ownership>, account: Ownership) -> Ownership {
    transaction.unwrap_or(account)
}

/// Body of `PUT /api/accounts/{id}/ownership`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetOwnership {
    pub ownership: Ownership,
}

/// Body of `POST /api/accounts/ownership` — attribute several accounts in one go.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetOwnershipBulk {
    pub account_ids: Vec<i64>,
    pub ownership: Ownership,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_round_trips_through_its_columns() {
        for value in [Ownership::Person { person_id: 7 }, Ownership::Joint] {
            let (ownership, person_id) = value.as_parts();
            assert_eq!(Ownership::from_stored(ownership, person_id), Ok(value));
        }
    }

    #[test]
    fn half_written_ownership_rows_are_errors_not_guesses() {
        assert!(Ownership::from_stored("person", None).is_err());
        assert!(Ownership::from_stored("joint", Some(1)).is_err());
        assert!(Ownership::from_stored("shared", None).is_err());
        // The state 0016 removed: a row still carrying it is a real error, not a value the
        // parser quietly re-admits.
        assert!(Ownership::from_stored("unattributed", None).is_err());
    }

    /// The wire shape the SPA consumes, and the one stored in a config snapshot.
    #[test]
    fn ownership_serialises_as_a_tagged_union() {
        let json = serde_json::to_value(Ownership::Person { person_id: 3 }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "person", "person_id": 3 })
        );
        assert_eq!(
            serde_json::to_value(Ownership::Joint).unwrap(),
            serde_json::json!({ "kind": "joint" })
        );
    }

    #[test]
    fn attribution_filters_parse_from_their_query_string_form() {
        assert_eq!("joint".parse(), Ok(Ownership::Joint));
        assert_eq!("7".parse(), Ok(Ownership::Person { person_id: 7 }));
        // Not silently ignored — an unparseable filter is a 400 at the edge.
        assert!("everyone".parse::<Ownership>().is_err());
        assert!("".parse::<Ownership>().is_err());
    }

    #[test]
    fn a_transaction_without_an_override_follows_its_account() {
        let account = Ownership::Person { person_id: 1 };
        assert_eq!(effective_ownership(None, account), account);
        // ...and an override wins, in either direction: one person's spend on the joint
        // card, or a shared expense on someone's own card.
        assert_eq!(
            effective_ownership(Some(Ownership::Joint), account),
            Ownership::Joint
        );
        assert_eq!(
            effective_ownership(Some(Ownership::Person { person_id: 2 }), Ownership::Joint),
            Ownership::Person { person_id: 2 }
        );
    }

    /// The hard requirement, at the outermost edge: a body that doesn't say who owns the
    /// account doesn't deserialise, so no handler ever gets the chance to default it.
    #[test]
    fn saving_an_account_without_an_owner_is_refused() {
        let body = serde_json::json!({
            "name": "Everyday",
            "kind": "bank",
            "currency_code": "NZD",
        });
        let err = serde_json::from_value::<crate::types::SaveAccount>(body).unwrap_err();
        assert!(err.to_string().contains("ownership"), "got {err}");
    }
}
