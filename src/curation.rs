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

pub const CURATED: &[Curated] = &[
    Curated {
        product: "csrf",
        instead: Instead::Nothing,
        why: "a command line is not a browser — /v1/csrf mints the anti-CSRF token a browser echoes back with a form POST, and nothing a person types has a form",
    },
    Curated {
        product: "openapi.json",
        instead: Instead::Nothing,
        why: "the document that decides the commands is not one of them",
    },
    Curated {
        product: "completions",
        instead: Instead::Claimed("chat completions"),
        why: "in a command line `completions` names SHELL completion; the operation is already reachable where it reads correctly, as `hanzo chat completions`",
    },
    // The three shadows. Each is a stated gap, not a claim that the routes are
    // absent: closing one means the local command ABSORBS the product's
    // operations, which is a UX decision per command, not an edit here.
    Curated {
        product: "code",
        instead: Instead::Claimed("code"),
        why: "`hanzo code` is the AI coding session and owns the bare name; it reaches none of the 6 live /v1/code routes, and closing that shadow means the local command absorbs them",
    },
    Curated {
        product: "billing",
        instead: Instead::Claimed("billing"),
        why: "`hanzo billing` is the prepaid-wallet wrapper with its own UX; it reaches 2 of the 22 live /v1/billing routes, and closing that shadow means the local command absorbs them",
    },
    Curated {
        product: "engine",
        instead: Instead::Claimed("engine"),
        why: "`hanzo engine serve <model>` runs an engine on THIS machine; the cloud engine product manages engine CLUSTERS and shadows 4 served operations until it takes its own noun or a nested home",
    },
    Curated {
        product: "help",
        instead: Instead::Nothing,
        why: "clap's own builtin `help` owns the name — a generated `help` product panics the parser with a duplicate command — so the served /v1/help operations are reachable by nothing",
    },
    Curated {
        product: "agent",
        instead: Instead::Nothing,
        why: "/v1/agent (one tool-calling round) and /v1/agents (the registry, sessions and targets) are two products sharing one noun, and the plural keeps it; the fix is a route move in hanzoai/agent (/v1/agent → /v1/agents/run), not a local command — none exists",
    },
    // Absorbed, losslessly. `machines`/`gpus` live at their own path prefixes with
    // a colliding `get`, so a FLAT `compute list` is impossible without ambiguity;
    // sub-namespacing makes the compute plane ONE command instead of three
    // top-levels. A flat surface would need the cloud specs reorganized under one
    // `/v1/compute` tag.
    Curated {
        product: "machines",
        instead: Instead::Under("compute"),
        why: "the compute plane is one command — every machine operation is reachable as `hanzo compute machines …`",
    },
    Curated {
        product: "gpus",
        instead: Instead::Under("compute"),
        why: "the compute plane is one command — every GPU operation is reachable as `hanzo compute gpus …`",
    },
];

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
