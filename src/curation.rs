//! Curation — the ONE table naming every product the command tree does not
//! surface at its own bare name, and why.
//!
//! Two readers, one table:
//!
//!   * `genproduct` APPLIES it — a curated product's operations are dropped, or
//!     relocated under another command.
//!   * `driftgate` EXCUSES it — a served product with no command is drift unless
//!     this table says otherwise, and the gate then counts how many times it had
//!     to say otherwise, and RUNS every spelling an entry claims.
//!
//! POLICY ONLY, and the test of an entry is ONE question: would it still hold if
//! the route were served perfectly? Each one answers yes, because each states a
//! fact about what a COMMAND LINE is — never a fact about what the server does.
//! Whether a route exists is the document's answer, made upstream by `genspec`
//! against cloud's own API document and downstream by `driftgate` against the
//! running host. THE CURATION LAW in `genproduct::main` enforces the first half:
//! it refuses to generate while any entry names a product `spec/cloud.json` does
//! not carry.
//!
//! A REASON THAT CANNOT BE FALSIFIED IS NOT A REASON — this table's second law,
//! and the one that made the reasons DATA instead of comments. Half of it used to
//! justify itself with "a local command owns this name", and two of those local
//! commands had been deleted, so 21 served `/v1/deploy` operations and 4
//! `/v1/agent` ones reached nobody while the list still called it a decision. Now
//! [`Instead::Claimed`] and [`Instead::Under`] name a spelling `driftgate` RUNS,
//! and an entry whose spelling stops resolving turns CI red. What is left over —
//! the surfaces nothing reaches — must say so in `why`, in words, where the count
//! makes it impossible to look away from.
//!
//! "A LOCAL COMMAND OWNS THIS NAME" IS NOT AN ENTRY AT ALL, and that is the third
//! law. It was three — `code`, `billing`, `engine` — and each admitted in its own
//! `why` that the routes are served and reached by nobody: 7 + 25 + 4 operations
//! curated out on the strength of a name clash. A clash is not a decision, it is
//! an ARRANGEMENT, and `commands::product::augment` now makes it one: the local
//! command ABSORBS the product of its own name, and inside it a local subcommand
//! owns its name the same way. One law at every level, no table. Only a fact that
//! survives the arrangement — a name a command line cannot spend at all — is an
//! entry here.
//!
//! It is compiled into the two GENERATOR binaries and never into `hanzo`: a
//! shipped CLI carries no policy about products it does not have.

// Each reader uses a different half — `genproduct` applies the placement and
// never reads a reason, `driftgate` reads every reason and applies nothing — so
// in either binary alone some of this table is legitimately untouched.
#![allow(dead_code)]

/// Where a curated product's surface goes instead.
pub enum Instead {
    /// Absorbed as `hanzo <parent> <product> …` — the operations keep their paths
    /// and only move coordinate. Nothing is lost.
    Under(&'static str),
    /// A DIFFERENT command answers to this bare name. The gate runs the spelling,
    /// so a name held for a command that no longer exists is drift, not policy.
    /// It does NOT claim the two serve the same routes — where a shadow leaves
    /// operations unreached, `why` says which and how many.
    Claimed(&'static str),
    /// Nothing reaches this surface. `why` says so plainly, in one sentence a
    /// person can act on.
    Nothing,
}

/// One curated product.
pub struct Curated {
    /// The `/v1/<product>` first segment this entry decides.
    pub product: &'static str,
    pub instead: Instead,
    /// One sentence, written for whoever finds the gap and needs to know whether
    /// it was a decision.
    pub why: &'static str,
}

pub const CURATED: &[Curated] = &[];

/// The entry deciding this product, if it is curated at all.
pub fn curated(product: &str) -> Option<&'static Curated> {
    CURATED.iter().find(|c| c.product == product)
}

/// The command a curated product's operations are absorbed under, if any.
pub fn under(product: &str) -> Option<&'static str> {
    match curated(product) {
        Some(Curated { instead: Instead::Under(parent), .. }) => Some(parent),
        _ => None,
    }
}

/// Curated OUT of the tree entirely: no command is generated for it, anywhere.
/// A product absorbed by [`Instead::Under`] is NOT dropped — it moves.
pub fn dropped(product: &str) -> bool {
    matches!(curated(product), Some(c) if !matches!(c.instead, Instead::Under(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two entries for one product is two policies for one decision, and
    /// [`curated`] would silently obey the first — so it cannot exist.
    #[test]
    fn one_entry_decides_one_product() {
        let mut seen: Vec<&str> = CURATED.iter().map(|c| c.product).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "a product is curated twice");
    }

    /// The reason is the entry. One whose `why` restates the product name, or says
    /// nothing, is the comment-shaped policy this table was rebuilt to be rid of —
    /// `driftgate` prints these to a person deciding whether a gap was deliberate,
    /// and a label does not help that person.
    #[test]
    fn every_entry_gives_a_reason_a_person_can_act_on() {
        for c in CURATED {
            let why = c.why.trim();
            assert!(
                why.len() > 30 && why.split_whitespace().count() > 5,
                "{}: `why` must be a sentence, not a label — got {why:?}",
                c.product
            );
            assert!(!why.eq_ignore_ascii_case(c.product), "{}: `why` restates the product name", c.product);
        }
    }

    /// A product cannot be absorbed under itself, and the parent must be a real
    /// name — `Under("")` would generate `hanzo  machines`.
    #[test]
    fn an_absorbed_product_lands_somewhere_else() {
        for c in CURATED {
            if let Instead::Under(parent) = c.instead {
                assert!(!parent.is_empty() && parent != c.product, "{}: absorbed into {parent:?}", c.product);
                assert!(under(c.product) == Some(parent) && !dropped(c.product));
            }
        }
    }
}
