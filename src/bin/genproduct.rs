//! `genproduct` — derive `src/commands/product/generated.rs` from `spec/cloud.json`
//! and nothing else. Offline and deterministic: the same spec always yields the
//! same tree, which is what lets `--check` be a build gate and what keeps `hanzo`
//! free of any runtime spec fetch.
//!
//! Source of truth: `spec/cloud.json`, ONE OpenAPI 3.1 document written by
//! `genspec` from hanzoai/cloud's own emitted `openapi.yaml` and nothing else.
//! Existence, prose and shape all come from that one reading, and none of the
//! three is restated here. Refresh the surface with `cargo run --features
//! genspec --bin genspec`, then run this.
//!
//! The fold from path → (product, resource nodes, verb, params) is TOTAL; typed
//! fields resolve $ref → component schema → property names + types + required.
//! It emits pure DATA: no host, no URL, no auth. See `commands::product`.
//!
//! Usage: `genproduct` writes the tree; `genproduct --check` regenerates it in
//! memory and fails if the committed file differs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

/// The ONE curation table — which products the tree does not surface at their own
/// bare name, which it absorbs under another command, and WHY, as data rather than
/// as a comment. `driftgate` reads the same file to excuse the same gaps and RUNS
/// every spelling an entry claims, so a product dropped here and a product excused
/// there can never be two different lists, and a reason cannot quietly stop being
/// true. It was three tables — `DENY`, `REMAP`, and an `EXCLUDE` that was a strict
/// subset of `DENY` — with the reasons in comments; two of `EXCLUDE`'s three names
/// reserved local commands that had been DELETED, and 21 served `/v1/deploy`
/// operations reached nobody while the list still called it a decision.
#[path = "../curation.rs"]
mod curation;

#[path = "genproduct_parts/schema.rs"]
mod schema;
use schema::*;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let spec_path = manifest.join("spec/cloud.json");
    let spec: Value = serde_json::from_str(&fs::read_to_string(&spec_path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — run `cargo run --features genspec --bin genspec`",
            spec_path.display()
        )
    }))
    .unwrap();
    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .expect("spec has no paths");

    // Per-product prose: the spec's `tags`, each description the owning Go
    // package's doc synopsis (lifted by cloud's weave, carried by genspec).
    // Whitespace is normalized to one line. Prose is display-only — it may
    // truthfully name a host or URL shape; the no-host invariant guards call
    // data, and the test scrubs prose before enforcing it.
    let mut tags: Vec<(String, String)> = spec
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let n = t.get("name")?.as_str()?.to_string();
            let d = t
                .get("description")?
                .as_str()?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            // Go doc convention opens with "Package <name> "; that identifier
            // belongs to Go's namespace, not the help line. Same rule as the
            // handler-name strip: drop the exact prefix, recapitalize, and a
            // connective "is/are" goes with it ("Package books is double-entry
            // accounting" -> "Double-entry accounting").
            // The strip is generic over the package WORD, not the tag name —
            // `audit` is package auditlog, `evals` package eval — any leading
            // "Package <word>" is the convention, whatever the word.
            let d = {
                let mut d = d;
                if let Some(rest) = d.strip_prefix("Package ") {
                    if let Some(sp) = rest.find(' ') {
                        let rest = &rest[sp + 1..];
                        let rest = rest
                            .strip_prefix("is ")
                            .or_else(|| rest.strip_prefix("are "))
                            .unwrap_or(rest);
                        let mut c = rest.chars();
                        d = match c.next() {
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                            None => rest.to_string(),
                        };
                    }
                }
                d
            };
            // The list line is ONE sentence, capped — clap renders it beside a
            // hundred siblings. A sentence ending in ":" was leading into a Go
            // doc's list; the colon dangles here, so it goes. The FULL prose
            // stays in the OpenAPI document and the MCP tool; the CLI's help
            // line is a label, not a manual.
            let d = {
                let mut s = match d.find(". ") {
                    Some(i) => d[..i + 1].to_string(),
                    None => d.clone(),
                };
                // The cap counts CHARACTERS, and `char_indices` is what makes it
                // one: a bare byte index can land inside a multi-byte char, and
                // slicing there aborts the generator instead of shortening a
                // sentence. clap lays this column out in characters anyway, so it
                // is also the number the cap meant.
                if let Some((cap, _)) = s.char_indices().nth(100) {
                    let cut = s[..cap].rfind(' ').unwrap_or(cap);
                    s = format!("{}…", s[..cut].trim_end_matches([',', ';', ':', '.']));
                }
                s.trim_end_matches(['.', ':']).trim().to_string()
            };
            (!d.is_empty()).then_some((n, d))
        })
        .collect();
    tags.sort();

    // The path universe the fold reads to tell a collection from an item and a
    // group from a leaf. It is the SERVED surface, so a sibling route that cloud
    // does not answer can no longer shape a command that it does.
    let all: BTreeSet<String> = paths
        .keys()
        .filter(|p| p.starts_with("/v1/"))
        .cloned()
        .collect();

    // Addresses answering more than one verb. A terminal noun at one of these
    // cannot be the command's verb without collapsing every method onto one
    // coordinate — see `fold`.
    let multi: BTreeSet<String> = paths
        .iter()
        .filter(|(p, _)| p.starts_with("/v1/"))
        .filter(|(_, item)| {
            item.as_object()
                .map(|o| o.keys().filter(|m| VERBS.contains(&m.as_str())).count() > 1)
                .unwrap_or(false)
        })
        .map(|(p, _)| p.trim_end_matches('/').to_string())
        .collect();

    // THE CURATION LAW: the tables may speak ONLY of products the document carries.
    // A name the spec does not mention states one thing and one thing only — that
    // the server does not serve it — and that is `genspec`'s answer against cloud's
    // own API document, arrived at by construction, not a list kept here. Without
    // this, every entry decays into the "this one 404s" knowledge deleting it cost
    // a release.
    let carried: BTreeSet<&str> = all.iter().filter_map(|p| segs(p).get(1).copied()).collect();
    let stale: Vec<&str> = curation::CURATED
        .iter()
        .map(|c| c.product)
        .filter(|n| !carried.contains(n))
        .collect();
    assert!(
        stale.is_empty(),
        "curation names {} product(s) spec/cloud.json does not carry: {}\n\n\
         A curation entry is a choice about what a COMMAND is, and must still hold if the route were \
         served perfectly. A name no document mentions asserts only that the server does not serve it \
         — which `genspec` decides against cloud's own API document. Delete the entry.",
        stale.len(),
        stale.join(" ")
    );

    let mut raw: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    let mut tunnels = 0usize;
    for (path, item) in paths {
        // A path key with a `?query` (AWS-S3-style sub-resource selectors) is
        // not a distinct RESOURCE — the query, not the path, distinguishes it.
        if !path.starts_with("/v1/") || path.contains('?') || path.contains('#') {
            continue;
        }
        let product0 = segs(path)[1];
        if curation::dropped(product0) || is_wild(product0) {
            continue;
        }
        for (m, op) in item.as_object().into_iter().flatten() {
            if !VERBS.contains(&m.as_str()) {
                continue;
            }
            let method = m.to_uppercase();
            let Some(f) = fold(&method, path, &all, &multi) else {
                continue;
            };
            // Curation remap: absorb a product UNDER another as a sub-namespace
            // (e.g. `machines list` → `compute machines list`). The PATH is
            // unchanged — only the command coordinate moves.
            let (mut product, mut nodes) = (f.product, f.nodes);
            if let Some(parent) = curation::under(&product) {
                nodes.insert(0, std::mem::replace(&mut product, parent.to_string()));
            }
            // Typed flags: body properties (writes) + query parameters (all ops).
            let write = matches!(method.as_str(), "POST" | "PUT" | "PATCH");
            let schema = write.then(|| body_schema(&spec, op)).flatten();
            let mut fields = schema
                .map(|s| fields_of(&spec, path, s))
                .unwrap_or_default();
            // WHY, decided here and nowhere else. `no-schema` is a TYPING GAP IN
            // CLOUD — the handler declares no JSON requestBody, so the document
            // states no shape and there is no shape for this generator to read.
            // `freeform` is a body the schema deliberately leaves open (`{}`,
            // `additionalProperties`, `oneOf`, a bare array), where `--data` is
            // the honest answer rather than a fallback.
            let untyped = match (write, schema, fields.is_empty()) {
                (true, None, _) => "no-schema",
                (true, Some(_), true) => "freeform",
                _ => "",
            };
            fields.extend(query_fields(&spec, op));
            // A body property that repeats a PATH parameter is the SAME value,
            // already supplied as a positional. It appears twice because zip binds
            // the path segment into the same Go struct as the body, so the emitted
            // schema honestly carries both. A flag for it would be a second way to
            // say one thing — and one that cannot work alone, the positional being
            // required.
            //
            // UNLESS it is the body's ONLY property. Then the two cannot be one
            // value: dropping it leaves a declared schema sending `{}`, and a
            // command whose whole body is typed away falls back to `--data`, which
            // is strictly less than the schema stated. `POST
            // /v1/o11y/service_accounts/{id}/roles` is the case — its `{id}` is the
            // service account and its body `id` is the ROLE to assign — and it was
            // the one `--data` write in the tree that no cloud typing gap explains.
            let echo = |fd: &FieldDef| f.params.contains(&fd.key);
            let body_survives = fields.iter().any(|fd| !fd.query && !echo(fd));
            fields.retain(|fd| !echo(fd) || (!body_survives && !fd.query));
            // One name may appear as BOTH a body property and a query param
            // (or twice after kebab-casing); a clap long must be unique, so
            // keep the FIRST (body wins over query).
            let mut seen_flag: BTreeSet<String> = BTreeSet::new();
            fields.retain(|f| seen_flag.insert(f.flag.clone()));
            // The multi-segment (catch-all) path params, as the ROUTER reports
            // them: genspec marks a param the server addresses with a fiber `*`,
            // so the runtime keeps its slashes raw instead of `%2F`-escaping them
            // into one opaque segment the backend 404s (see fill_path).
            let rest: Vec<String> = op
                .get("x-catch-all")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let sum = op
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // A METHOD-OVERRIDE TUNNEL IS NOT A COMMAND. cloud registers a POST
            // beside the real verb at some addresses so a browser form — which can
            // only send GET and POST — can still reach a PUT/PATCH/DELETE handler.
            // Its own summary says exactly that. A CLI sends the real verb, so the
            // tunnel reaches nothing this tree does not already reach at the verb
            // it tunnels for, and generating it would put two spellings of one
            // command in the surface.
            //
            // This is a DROP, not an elision: nothing is lost, so it must not be
            // counted as loss. It is counted separately and written into the
            // generated header, because a number that changes without anyone
            // noticing is how the elision census went unread in the first place.
            if sum.starts_with("Method-override tunnel") {
                tunnels += 1;
                continue;
            }
            let coord = (product.clone(), nodes.clone(), f.verb.clone());
            raw.entry(coord).or_default().push(Op {
                product,
                nodes,
                verb: f.verb,
                method,
                path: curation::served_at(path).to_string(),
                params: f.params,
                rest,
                fields,
                sum,
                untyped,
            });
        }
    }

    // Collision resolution — ARITY only. When two ops fold to the same coordinate
    // with different positional counts (`GET /v1/mq/objects` vs
    // `GET /v1/mq/objects/{store}/list`), the MAX-arity op keeps the verb and the
    // shallower one becomes `<verb>-all`.
    //
    // A group/leaf coincidence (a collection-root verb that also names a child
    // group, e.g. `GET /v1/kv` = `list` while `/v1/kv/list/{key}` nests a `list`
    // group) is NOT renamed: the op lands as a leaf on the SAME node as the group,
    // and the runtime makes that node a RUNNABLE GROUP (`hanzo kv list` runs the
    // collection GET; `hanzo kv list push <key>` runs the datatype). Keeping the
    // collection GET a runnable leaf is the whole point.
    let mut resolved: BTreeMap<(String, Vec<String>, String), Vec<Op>> = BTreeMap::new();
    for ((p, nodes, verb), ops) in raw {
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        if arities.len() <= 1 {
            resolved.entry((p, nodes, verb)).or_default().extend(ops);
            continue;
        }
        let maxar = *arities.iter().max().unwrap();
        for mut o in ops {
            // Rename the op's OWN verb, not just the map key — the emitted data
            // must carry the disambiguated verb.
            if o.params.len() != maxar {
                o.verb = format!("{verb}-all");
            }
            let coord = (p.clone(), nodes.clone(), o.verb.clone());
            resolved.entry(coord).or_default().push(o);
        }
    }

    // Collapse multi-method coordinates by priority (one op per command) — and
    // COUNT what that costs. The fold names a leaf after its own noun, so two
    // methods at one address want one name: `PUT` and `PATCH` on an item both
    // read as `update`, `GET` and `POST` on `/v1/billing/payment-methods` both
    // read as `payment-methods`. One of them wins and the other reaches nobody.
    //
    // "Is a `PUT` beside a `PATCH` a different command, or the same one?" used to
    // stand here as a question this generator could not answer. It was never the
    // generator's to answer, and it was never open: cloud states it, per address,
    // in the prose it publishes. `PUT /v1/store/{storeid}` is "Replace a storefront
    // outright"; `PATCH` is "Change part of a storefront". Two commands, said in
    // the contract, folded into one name by a table here — so `root_verb` and
    // `item_verb` now separate them, and the same reading retired the 18
    // method-override tunnels, which cloud's own summary calls a shim for clients
    // that cannot send the real verb.
    //
    // What survives the census is genuinely undecided, and the rule is unchanged:
    // an operation that vanishes without a number is exactly the shape of defect
    // this pipeline exists to end. So the loss is reported, and pinned as a
    // CEILING — free to fall, and it cannot grow without somebody deciding.
    let mut coords: Vec<Op> = Vec::new();
    let mut elided: Vec<Op> = Vec::new();
    for (_c, mut ops) in resolved {
        ops.sort_by_key(|o| method_rank(&o.method));
        let arities: BTreeSet<usize> = ops.iter().map(|o| o.params.len()).collect();
        assert!(
            arities.len() == 1,
            "unresolved arity collision: {:?}",
            ops.iter().map(|o| &o.path).collect::<Vec<_>>()
        );
        let mut it = ops.into_iter();
        coords.push(it.next().expect("a coordinate holds at least one op"));
        elided.extend(it);
    }
    elided.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    coords.sort_by(|a, b| (&a.product, &a.nodes, &a.verb).cmp(&(&b.product, &b.nodes, &b.verb)));

    // Guard: the runtime tree needs unique (product, nodes, verb) — fail loudly.
    let mut seen_coord: BTreeSet<(String, Vec<String>, String)> = BTreeSet::new();
    for o in &coords {
        let c = (o.product.clone(), o.nodes.clone(), o.verb.clone());
        assert!(
            seen_coord.insert(c),
            "DUP COORD: {} {:?} {} <- {}",
            o.product,
            o.nodes,
            o.verb,
            o.path
        );
    }

    // Guard: prose is not optional, and there is no substitute for it. A command
    // whose help line would be blank is a MISSING GO DOC COMMENT on the handler in
    // hanzoai/cloud — zipdoc lifts that comment into the published route table,
    // genspec joins it in, and this generator only carries it. Printing something
    // else (the route, the verb, a manufactured phrase) would convert a fixable
    // gap at the source into a shipped help line that reads deliberate, and nobody
    // files a bug against a design choice. So the generator REFUSES to emit the
    // command at all: the gap stays visible at the one place it can be closed.
    let bare: Vec<String> = coords
        .iter()
        .filter(|o| o.sum.trim().is_empty())
        .map(|o| {
            let mut c = vec![o.product.clone()];
            c.extend(o.nodes.iter().cloned());
            c.push(o.verb.clone());
            format!("  hanzo {}\n      <- {} {}", c.join(" "), o.method, o.path)
        })
        .collect();
    assert!(
        bare.is_empty(),
        "{} operation(s) in spec/cloud.json carry no summary, so these commands would have \
         nothing to say:\n{}\n\n\
         Every command states what it does for the person running it. That sentence is the Go \
         doc comment on the handler in hanzoai/cloud: zipdoc lifts it into the published route \
         table, genspec joins it into spec/cloud.json, and genproduct carries it here. NOTHING \
         IN THIS REPO CAN SUPPLY IT — write the doc comment where the handler lives, run `make \
         openapi` in hanzoai/cloud, then re-run `cargo run --features genspec --bin genspec`.",
        bare.len(),
        bare.join("\n"),
    );

    // The elision census, read beside the ops it stands for. It is printed every
    // run and written into the generated header, because a gap nobody prints is a
    // gap nobody closes.
    let by_product = elided
        .iter()
        .fold(BTreeMap::<&str, usize>::new(), |mut m, o| {
            *m.entry(o.product.as_str()).or_default() += 1;
            m
        });
    let worst = {
        let mut v: Vec<_> = by_product.iter().collect();
        v.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), **p));
        v.into_iter()
            .take(8)
            .map(|(p, n)| format!("{p} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    eprintln!(
        "genproduct: {} served operation(s) share a coordinate with a higher-priority method and \
         reach no command ({worst}{})",
        elided.len(),
        if by_product.len() > 8 { ", …" } else { "" }
    );
    eprintln!(
        "genproduct: {tunnels} method-override tunnel(s) dropped — a POST cloud registers so a \
         browser form can reach a PUT/PATCH/DELETE handler. Nothing is lost: a CLI sends the real \
         verb, and that command is in the tree."
    );

    // THE UNTYPED CENSUS. A `--data` write is the CLI saying it does not know the
    // shape, and until now the only number anywhere was one aggregate in the
    // generated header, which cannot say WHOSE gap it is. It is two different
    // facts and they have two different owners:
    //
    //   no-schema  the handler in hanzoai/cloud declares no JSON requestBody, so
    //              the document states no shape. NOTHING IN THIS REPO CAN CLOSE
    //              IT — the fix is a typed op where the handler lives (#67).
    //   freeform   the schema is open BY CONSTRUCTION (`{}`, additionalProperties,
    //              oneOf, a bare array). `--data` is the honest answer, not a
    //              fallback, and typing it would be inventing a shape.
    //
    // Printed every run, and the per-product split is the hand-off list, so the
    // number a cloud owner needs is the generator's own output rather than an
    // analysis somebody has to redo.
    let untyped: Vec<&Op> = coords.iter().filter(|o| !o.untyped.is_empty()).collect();
    let no_schema = untyped.iter().filter(|o| o.untyped == "no-schema").count();
    let gap_by_product = untyped.iter().filter(|o| o.untyped == "no-schema").fold(
        BTreeMap::<&str, usize>::new(),
        |mut m, o| {
            *m.entry(o.product.as_str()).or_default() += 1;
            m
        },
    );
    let gap_worst = {
        let mut v: Vec<_> = gap_by_product.iter().collect();
        v.sort_by_key(|(p, n)| (std::cmp::Reverse(**n), **p));
        v.into_iter()
            .take(8)
            .map(|(p, n)| format!("{p} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    eprintln!(
        "genproduct: {} write command(s) take --data — {no_schema} because the handler declares no \
         requestBody in hanzoai/cloud ({gap_worst}{}), {} because the schema is freeform by \
         construction",
        untyped.len(),
        if gap_by_product.len() > 8 { ", …" } else { "" },
        untyped.len() - no_schema,
    );

    // ---- emit ----
    // `spec/cloud.json` is the ONLY source: every cloud capability is a real
    // `hanzo <product> <resource> <verb>`. A product the spec does not carry is a
    // product no cloud router registers, because the spec is a projection of
    // cloud's own emitted document and of nothing else. There is no passthrough
    // and no `hanzo api` fallback to paper over it: that gap closes by SERVING
    // the route, which is the only place it can close.
    let ntyped = coords.iter().filter(|o| !o.fields.is_empty()).count();
    let ndata = coords
        .iter()
        .filter(|o| o.fields.is_empty() && matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
        .count();
    let nprod = coords
        .iter()
        .map(|o| &o.product)
        .collect::<BTreeSet<_>>()
        .len();

    let mut s = String::new();
    s.push_str("//! @generated by `cargo run --bin genproduct` from `spec/cloud.json`\n");
    s.push_str(
        "//! (itself a projection of hanzoai/cloud's emitted openapi.yaml, and of nothing else).\n",
    );
    s.push_str("//! DO NOT EDIT BY HAND — `cargo test` regenerates and diffs this.\n//!\n");
    s.push_str("//! Pure DATA: (product, resource nodes, verb, method, /v1 path, params, typed\n");
    s.push_str("//! body fields). No host, no absolute URL, no auth — pinned by a test.\n//!\n");

    // THE CENSUS RIDES IN THE FILE IT DESCRIBES. Every number here was once a
    // `const` a person edited by hand on each re-pin, so the pipeline that re-pins
    // nightly could not land one: the constants move whenever cloud moves, and a
    // machine has no hand. They are DERIVED facts about the pinned document, and a
    // derived fact belongs in the projection — where `genproduct --check` already
    // diffs this file byte for byte, so a census that moves turns CI red and shows
    // the delta in the diff a person reads. One mechanism, and it needs no editing
    // to stay true.
    s.push_str("//! Census of this derivation, against the document `.spec-lock` names:\n");
    s.push_str(&format!(
        "//!   {} write command(s) take --data — {no_schema} because the handler in hanzoai/cloud\n\
         //!   declares no requestBody ({gap_worst}{}), {} because the schema is freeform by\n\
         //!   construction. Only the first is a gap, and it closes where the handler lives.\n",
        untyped.len(),
        if gap_by_product.len() > 8 { ", …" } else { "" },
        untyped.len() - no_schema,
    ));
    s.push_str(&format!(
        "//!   {tunnels} method-override tunnel(s) dropped — a POST cloud registers so a browser\n\
         //!   form can reach a PUT/PATCH/DELETE handler. Nothing is lost: a CLI sends the real verb.\n",
    ));
    s.push_str(&format!(
        "//!   {} served operation(s) share a coordinate with a higher-priority method and reach\n\
         //!   no command. Two methods at one address want one command name, so one of them reaches\n\
         //!   nobody; the fold's `has_child` branch is how a second one gets a name of its own.\n",
        elided.len(),
    ));
    for o in &elided {
        s.push_str(&format!("//!     {} {}\n", o.method, o.path));
    }
    s.push('\n');
    s.push_str("use super::{Field, Op, Ty};\n\n");
    s.push_str(&format!(
        "/// {} coordinates across {} products ({} typed-flag, {} --data-fallback writes).\n",
        coords.len(),
        nprod,
        ntyped,
        ndata
    ));
    s.push_str("pub(crate) static OPS: &[Op] = &[\n");
    for o in &coords {
        s.push_str(&emit_op(o));
    }
    s.push_str("];\n");

    // One line per product that documents itself — the group's help line. Only
    // products that actually field commands; a tag for an excluded product is
    // dropped with it.
    let live: BTreeSet<&String> = coords.iter().map(|o| &o.product).collect();
    s.push_str("\n/// Each product's own prose (its package doc synopsis), for the group help.\n");
    s.push_str("pub(crate) static PRODUCTS: &[(&str, &str)] = &[\n");
    for (n, d) in tags.iter().filter(|(n, _)| live.contains(n)) {
        s.push_str(&format!("    ({n:?}, {d:?}),\n"));
    }
    s.push_str("];\n");

    let out = manifest.join("src/commands/product/generated.rs");
    // --check is the drift gate (see tests/spec_drift.rs). The derivation runs
    // either way; the flag only decides whether the answer is written or compared,
    // so the gate can never test a different derivation than the one that writes.
    if std::env::args().any(|a| a == "--check") {
        let got = fs::read_to_string(&out).unwrap_or_default();
        if got != s {
            eprintln!(
                "genproduct --check: {} is not what spec/cloud.json derives ({} vs {} bytes). \
                 Run `cargo run --bin genproduct` and commit the result.",
                out.display(),
                got.len(),
                s.len()
            );
            std::process::exit(1);
        }
        eprintln!(
            "genproduct --check: {} ops match spec/cloud.json",
            coords.len()
        );
        return;
    }
    fs::write(&out, s).unwrap();
    eprintln!(
        "genproduct: {} ops ({} typed, {} data-fallback) across {} products -> {}",
        coords.len(),
        ntyped,
        ndata,
        nprod,
        out.display()
    );
}

fn emit_slice(items: &[String]) -> String {
    format!(
        "&[{}]",
        items
            .iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn emit_op(o: &Op) -> String {
    let fields = if o.fields.is_empty() {
        "&[]".to_string()
    } else {
        let items: Vec<String> = o
            .fields
            .iter()
            .map(|f| {
                // The clap id is namespaced by LOCATION so a body property and a
                // query param of the same name never collide.
                let id = format!("{}.{}", if f.query { "query" } else { "field" }, f.key);
                format!(
                    "Field {{ key: {:?}, id: {:?}, flag: {:?}, ty: Ty::{}, required: {}, choices: {}, query: {}, secret: {}, repeat: {} }}",
                    f.key,
                    id,
                    f.flag,
                    f.ty,
                    f.required,
                    emit_slice(&f.choices),
                    f.query,
                    f.secret,
                    f.repeat
                )
            })
            .collect();
        format!("&[{}]", items.join(", "))
    };
    format!(
        "    Op {{ product: {:?}, nodes: {}, verb: {:?}, method: {:?}, path: {:?}, params: {}, rest: {}, fields: {}, sum: {:?} }},\n",
        o.product,
        emit_slice(&o.nodes),
        o.verb,
        o.method,
        o.path,
        emit_slice(&o.params),
        emit_slice(&o.rest),
        fields,
        o.sum
    )
}

#[cfg(test)]
mod tests {
    use super::{is_secret, query_fields};
    use serde_json::json;

    fn s(name: &str, ty: &str) -> bool {
        is_secret("/v1/x", name, &json!({ "type": ty }))
    }

    /// The standard marker still decides, on its own, whatever the field is called.
    #[test]
    fn the_openapi_marker_is_honoured_first() {
        assert!(is_secret(
            "/v1/x",
            "anything",
            &json!({ "type": "string", "format": "password" })
        ));
    }

    /// …and is not TRUSTED on its own. Cloud's document carries `format: password`
    /// on zero fields while serving sign-in passwords, client secrets, API keys and
    /// bearer tokens. When the single upstream marker went away, so did every
    /// protection that depended on it — silently, which is the failure this exists
    /// to make impossible.
    #[test]
    fn a_credential_is_recognised_by_name_too() {
        for name in [
            "password",
            "newPassword",
            "oldPassword",
            "defaultPassword",
            "masterPassword",
            "secret",
            "clientSecret",
            "accessSecret",
            "token",
            "accessToken",
            "refreshToken",
            "apiKey",
            "privateKey",
            "passphrase",
            "credential",
        ] {
            assert!(
                s(name, "string"),
                "`{name}` carries a credential and must go to stdin"
            );
        }
    }

    /// A name that DESCRIBES a credential is not one. Forcing these onto stdin
    /// would be noise, and noise is what teaches people to distrust the mechanism.
    #[test]
    fn naming_a_credential_is_not_carrying_one() {
        for name in [
            "passwordSalt",
            "passwordHash",
            "password_file",
            "tokenFormat",
            "tokenSigningMethod",
            "secretRef",
            "credentialId",
            "tokenPrefix",
            "passwordExpireDays",
            "secretName",
            "tokenUrl",
        ] {
            assert!(
                !s(name, "string"),
                "`{name}` names a credential without being one"
            );
        }
    }

    /// Only a string can BE the value: a boolean `secret` is a switch, an integer
    /// `budgetTokens` is a budget, an array `tokenFields` is a schema.
    #[test]
    fn only_a_string_can_be_the_value() {
        for (name, ty) in [
            ("secret", "boolean"),
            ("budgetTokens", "integer"),
            ("tokenFields", "array"),
        ] {
            assert!(!s(name, ty), "`{name}: {ty}` is not a secret value");
        }
    }

    fn on(path: &str, name: &str) -> bool {
        is_secret(path, name, &json!({ "type": "string" }))
    }

    /// The two operations that STORE a secret call the field `value`, a name no
    /// name rule can catch, and the document carries `format: password` on no
    /// field. The operation is the evidence: a `value` written to a `secrets`
    /// route is the secret.
    #[test]
    fn the_value_stored_at_a_secrets_route_is_a_secret() {
        assert!(on("/v1/environment/{repo}/secrets/{name}", "value"));
    }

    #[test]
    fn the_value_of_a_new_kms_secret_is_a_secret() {
        assert!(on("/v1/kms/secrets", "value"));
    }

    #[test]
    fn the_value_of_a_notify_credential_is_a_secret() {
        assert!(on("/v1/notify/credentials/{provider}/{key}", "value"));
    }

    /// A `credentials` route whose body has no `value` is untouched: the route
    /// alone is not evidence, the field called `value` on it is.
    #[test]
    fn a_credentials_route_without_a_value_field_is_unaffected() {
        assert!(!on("/v1/social/credentials", "clientId"));
    }

    /// A `value` anywhere else is just a value: a form field is not a credential.
    #[test]
    fn a_value_elsewhere_is_not_a_secret() {
        assert!(!on(
            "/v1/esign/o/{org}/sign/{token}/fields/{fieldId}",
            "value"
        ));
    }

    /// A query parameter rides the URL and is never a stdin-secret, whatever
    /// the route is called.
    #[test]
    fn a_query_value_is_never_a_secret() {
        let op = json!({ "parameters": [
            { "name": "value", "in": "query", "schema": { "type": "string" } }
        ]});
        let f = query_fields(&json!({}), &op);
        assert_eq!(f.len(), 1);
        assert!(!f[0].secret);
    }
}
