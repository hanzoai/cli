//! The generated command table's TYPES — pure data, and nothing that acts on it.
//!
//! They live apart from the parser for one reason: `generated.rs` is the CLI's
//! whole cloud surface as a value, and TWO readers need that value. `hanzo`
//! builds a clap tree out of it; `driftgate` asks the live server whether every
//! coordinate in it is still served, and has to be able to NAME the command a
//! failing coordinate belongs to. A gate that re-parses the generated file, or
//! restates these structs, is a second definition of one shape — the exact
//! disease this pipeline exists to end, one layer down.

/// One generated operation — pure DATA derived from the authored spec SHAPE.
pub struct Op {
    /// First path segment after `/v1/` — the top-level command.
    pub product: &'static str,
    /// Resource group chain (nouns), verb excluded — the subcommand path.
    pub nodes: &'static [&'static str],
    /// The leaf verb (`list`/`get`/`create`/`update`/`rm`, or an action name).
    pub verb: &'static str,
    /// The one HTTP method this coordinate dispatches with.
    pub method: &'static str,
    /// The `/v1` path template, e.g. `/v1/agents/sessions/{id}/events`.
    pub path: &'static str,
    /// User positionals, in path order — the tenant-org scope is NOT among them.
    pub params: &'static [&'static str],
    /// The subset of `params` that address a MULTI-SEGMENT path: a server
    /// catch-all whose value is a `/`-joined address (e.g. a KMS secret
    /// `sub/path/name`). Their `/` is STRUCTURAL — each segment is percent-encoded
    /// but the slashes ride raw, so the route resolves; `.`/`..`/empty segments are
    /// refused. Empty for the single-segment default, where a `/` in a value is
    /// `%2F`-escaped so it can never re-address a different route.
    pub rest: &'static [&'static str],
    /// Typed request-body fields from the requestBody schema. Empty ⇒ no typed
    /// body (a write with no schema uses `--data`; a read has no body).
    pub fields: &'static [Field],
    /// One line of prose from the spec — a typed op's doc comment, lifted by
    /// zipdoc, carried by genspec. NEVER empty: `genproduct` refuses to emit an
    /// undescribed op, so a command with nothing to say never becomes one.
    pub sum: &'static str,
}

/// One typed request-body field, from a schema property.
pub struct Field {
    /// The JSON property name — the body key sent to cloud. DOTTED for a nested
    /// property (`spec.replicas`), which [`insert_path`] rebuilds into the object
    /// the schema declared. No property name in cloud's document contains a `.`,
    /// so the path is unambiguous.
    pub key: &'static str,
    /// The clap arg id — namespaced (`field.<key>`) so it can never collide with a
    /// path positional or a fixed control, even when a body key is `data`/`id`.
    pub id: &'static str,
    /// The kebab-case long flag the user types (`amountCents` → `--amount-cents`).
    pub flag: &'static str,
    /// The property's type, which picks the clap parser + the JSON encoding.
    pub ty: Ty,
    /// A `required` property is a required flag; else it is omitted when unset.
    pub required: bool,
    /// A string enum's allowed values (clap validates); empty otherwise.
    pub choices: &'static [&'static str],
    /// `true` ⇒ a URL query-string parameter (`?key=…`); `false` ⇒ a requestBody
    /// property. The flag looks identical; only the destination differs.
    pub query: bool,
    /// A SECRET body value (`format: password` in the spec). It gets NO flag and
    /// NO positional — the value is read from STDIN at dispatch through the one
    /// secret law (`iam::secret::read_secret`), so it can never land in argv,
    /// `ps` or shell history. The grammar refuses a value-bearing argument.
    pub secret: bool,
    /// The schema said ARRAY: the flag may be given more than once and the values
    /// collect into a JSON array (`--tag a --tag b` → `["a","b"]`). `ty` is then
    /// the ELEMENT's type, so an array of strings is a repeatable `--tag STRING`
    /// rather than one opaque `--tag '["a","b"]'` the shell has to quote.
    pub repeat: bool,
}

/// The JSON type of a body field — schema `type` mapped to a clap parser.
pub enum Ty {
    Str,
    Int,
    Num,
    Bool,
    Json,
}

impl Op {
    pub fn is_write(&self) -> bool {
        matches!(self.method, "POST" | "PUT" | "PATCH")
    }
}
