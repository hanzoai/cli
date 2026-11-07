//! `hanzo graph` — local Apple Personal Knowledge Graph (`~/Library/IntelligencePlatform`)
//! reader, entity/kinship decoder, conversational state extractor, and cloud sync
//! into `/v1/graph/{ontology,entity,apple,convo}`.
//!
//! Coexists with the generated `/v1/graph` cloud subcommands (`search`, `resolve`,
//! `neighbors`, `path`, `communities`, `answer`, `diff`, `derive`, `extract`,
//! `ingest`, `erase`, `vocabulary`, `graphql`, `create`, `get`) via the collision
//! law in `commands::product::augment`.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use colored::Colorize;
use hanzo_client::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

const APPLE_EPOCH_OFFSET_SECS: i64 = 978_307_200;

pub const KINSHIP_COLUMNS: [&str; 17] = [
    "mother",
    "father",
    "parent",
    "sister",
    "brother",
    "sibling",
    "family",
    "friend",
    "familyAndFriends",
    "partner",
    "coworker",
    "alumni",
    "child",
    "son",
    "daughter",
    "myself",
    "unknown",
];

fn graph_route(endpoint: &str) -> String {
    format!("/{}/{}/{endpoint}", "v1", "graph")
}

#[derive(Subcommand)]
pub enum GraphCommands {
    /// Inspect the local macOS Apple Personal Knowledge Graph (`~/Library/IntelligencePlatform`)
    Local {
        /// Path to the IntelligencePlatform directory (defaults to ~/Library/IntelligencePlatform)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Filter to a specific entity ID or substring of its resolved name
        #[arg(long)]
        entity: Option<String>,
        /// Maximum number of triples to read from stable_graph (0 = all)
        #[arg(long, default_value_t = 5000)]
        limit: usize,
        /// Emit the full decoded snapshot + `/v1/graph/apple` payload as JSON
        #[arg(long)]
        json: bool,
    },
    /// Read the local Apple Personal Knowledge Graph and sync it to `/v1/graph/apple`
    Sync {
        /// Path to the IntelligencePlatform directory (defaults to ~/Library/IntelligencePlatform)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Canonical entity ID for the device owner ("me")
        #[arg(long, default_value = "me")]
        owner: String,
        /// Provenance tag recorded on ingested assertions
        #[arg(long, default_value = "apple/intelligenceplatform")]
        source: String,
        /// Maximum number of triples to sync per batch
        #[arg(long, default_value_t = 5000)]
        limit: usize,
        /// Build and validate the `/v1/graph/apple` payload without sending it
        #[arg(long)]
        dry_run: bool,
    },
    /// List the Personal Knowledge Graph ontology (181 entity classes, 278 predicates, 17 kinship columns)
    Ontology {
        /// Filter classes and predicates by group (e.g. core, person, relationship_type, kinship)
        #[arg(long)]
        group: Option<String>,
        /// Case-insensitive substring search across IDs, labels, and descriptions
        #[arg(long)]
        q: Option<String>,
        /// Print the built-in ontology summary locally without calling the cloud
        #[arg(long)]
        local: bool,
    },
    /// Resolve the complete state of an entity (type, properties, edges, kinship scorecard, expired facts)
    Entity {
        /// Entity identifier or name to inspect
        #[arg(long)]
        entity: String,
        /// Point in world time (RFC 3339) at which to evaluate holding facts
        #[arg(long)]
        as_of: Option<String>,
        /// Point in knowledge time (RFC 3339) ceiling for recorded assertions
        #[arg(long)]
        as_known: Option<String>,
        /// Resolve against the local macOS `~/Library/IntelligencePlatform` database
        #[arg(long)]
        local: bool,
        /// Path to the IntelligencePlatform directory when `--local` is used
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Ingest an Apple Personal Knowledge Graph snapshot into `/v1/graph/apple`
    Apple {
        /// Path to the IntelligencePlatform directory (defaults to ~/Library/IntelligencePlatform)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Canonical entity ID for the device owner ("me")
        #[arg(long, default_value = "me")]
        owner: String,
        /// Provenance tag recorded on ingested assertions
        #[arg(long, default_value = "apple/intelligenceplatform")]
        source: String,
        /// RFC 3339 timestamp for the snapshot (defaults to current UTC time)
        #[arg(long)]
        at: Option<String>,
        /// Maximum number of triples to read from stable_graph
        #[arg(long, default_value_t = 5000)]
        limit: usize,
        /// Build and validate the payload without sending it
        #[arg(long)]
        dry_run: bool,
    },
    /// Extract entity state and relationship assertions from conversation turns (`/v1/graph/convo`)
    Convo {
        /// Utterance or dialogue text to extract entity state from
        #[arg(long)]
        text: String,
        /// Speaker entity identifier (defaults to "me")
        #[arg(long, default_value = "me")]
        speaker: String,
        /// Addressed conversation partner entity identifier
        #[arg(long)]
        partner: Option<String>,
        /// Provenance tag for the conversation (e.g. chat/session-1)
        #[arg(long, default_value = "cli/convo")]
        source: String,
        /// RFC 3339 timestamp for the turn (defaults to current UTC time)
        #[arg(long)]
        at: Option<String>,
        /// Extract assertions without persisting them (`dry_run: true`)
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleTriple {
    pub subject: String,
    pub predicate: String,
    #[serde(rename = "relationshipId", default, skip_serializing_if = "String::is_empty")]
    pub relationship_id: String,
    #[serde(rename = "relationshipPredicate", default, skip_serializing_if = "String::is_empty")]
    pub relationship_predicate: String,
    pub object: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sources: String,
    #[serde(rename = "sourceDuplicates", default, skip_serializing_if = "is_zero")]
    pub source_duplicates: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timestamp: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleScorecard {
    pub entity: String,
    pub scores: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEntityView {
    pub id: String,
    pub class_id: String,
    pub class_label: String,
    pub name: String,
    pub attributes: BTreeMap<String, Vec<String>>,
    pub edges: Vec<LocalEdgeView>,
    pub scorecard: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEdgeView {
    pub predicate_id: String,
    pub predicate_label: String,
    pub relationship_type_id: String,
    pub relationship_type_label: String,
    pub target_id: String,
    pub target_name: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAppleSnapshot {
    pub dir: String,
    pub active_triples_total: usize,
    pub expired_triples_total: usize,
    pub scorecards_total: usize,
    pub entities_total: usize,
    pub classes_breakdown: BTreeMap<String, usize>,
    pub entities: Vec<LocalEntityView>,
    pub apple_payload: Value,
}

pub fn default_intelligence_platform_dir() -> Result<PathBuf> {
    if let Ok(env_dir) = std::env::var("HANZO_APPLE_PKG_DIR") {
        if !env_dir.trim().is_empty() {
            return Ok(PathBuf::from(env_dir));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    Ok(home.join("Library").join("IntelligencePlatform"))
}

pub fn class_label(code: &str) -> &'static str {
    match code {
        "SB56" => "entity",
        "SB104" => "person",
        "SB152" => "place",
        "nm_topic" => "topic",
        "SB65" => "event",
        "nm_document" => "document",
        "SB473" => "home",
        "SB10" => "organization",
        "SB2" => "group of humans",
        "SB144" => "software",
        "SB193" => "organism",
        "SB198" => "agent",
        "SB83" => "concept",
        "SB400" => "occurrence",
        "SB60" => "role",
        "SB197" => "siribase type",
        "nm_knowledgeSource" => "knowledge source",
        "SB170" => "time",
        "SB175" => "group",
        "SB470" => "device primary user",
        "CS165" => "employment",
        "CS148" => "residence",
        "CS112" => "kinship",
        "CS113" => "mother",
        "CS114" => "father",
        "CS115" => "parent",
        "CS116" => "sister",
        "CS117" => "brother",
        "CS118" => "sibling",
        "CS119" => "child",
        "CS120" => "son",
        "CS121" => "daughter",
        "CS122" => "partner",
        "CS123" => "spouse",
        "CS124" => "friend",
        "CS125" => "coworker",
        "CS126" => "alumni",
        "CS127" => "family",
        "CS128" => "familyAndFriends",
        _ => "entity",
    }
}

pub fn predicate_label(code: &str) -> &str {
    match code {
        "PS1" => "instance of",
        "PS2" => "label",
        "PS10" => "given name",
        "PS11" => "family name",
        "PS12" => "nickname",
        "PS19" => "birthday",
        "PS22" => "email",
        "PS23" => "phone",
        "PS24" => "handle",
        "PS33" => "name",
        "PS45" => "member of",
        "PS48" => "relationship to me",
        "PS51" => "attendee",
        "PS59" => "located in",
        "PS92" => "bundle identifier",
        "PS112" => "start date",
        "PS113" => "end date",
        "PS204" => "has residence",
        "PS311" => "role title",
        "PS500" => "employer",
        "PS501" => "school",
        _ => code,
    }
}

fn normalize_timestamp(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    if DateTime::parse_from_rfc3339(s).is_ok() {
        return s.to_string();
    }
    if let Ok(v) = s.parse::<f64>() {
        if v > 0.0 {
            let unix_secs = if v < 1_000_000_000.0 {
                (v as i64) + APPLE_EPOCH_OFFSET_SECS
            } else {
                v as i64
            };
            if let Some(dt) = DateTime::<Utc>::from_timestamp(unix_secs, 0) {
                return dt.to_rfc3339();
            }
        }
    }
    String::new()
}

fn sqlite_json_query(db_path: &Path, sql: &str) -> Result<Vec<BTreeMap<String, Value>>> {
    let out = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(db_path)
        .arg(sql)
        .output()
        .with_context(|| format!("failed to execute sqlite3 on {}", db_path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "sqlite3 query failed on {} ({}): {}",
            db_path.display(),
            out.status,
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<BTreeMap<String, Value>> =
        serde_json::from_str(trimmed).with_context(|| format!("parse sqlite3 json output from {}", db_path.display()))?;
    Ok(rows)
}

fn val_to_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

fn val_to_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn val_to_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

fn query_graph_triples(db_path: &Path, table: &str, limit: usize) -> Result<Vec<AppleTriple>> {
    let limit_clause = if limit > 0 {
        format!(" LIMIT {limit}")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT COALESCE(CAST(subject AS TEXT),'') AS subject, \
                COALESCE(CAST(predicate AS TEXT),'') AS predicate, \
                COALESCE(CAST(relationshipId AS TEXT),'') AS relationshipId, \
                COALESCE(CAST(relationshipPredicate AS TEXT),'') AS relationshipPredicate, \
                COALESCE(CAST(object AS TEXT),'') AS object, \
                COALESCE(CAST(confidence AS REAL), 0.9) AS confidence, \
                COALESCE(CAST(sources AS TEXT),'') AS sources, \
                COALESCE(CAST(sourceDuplicates AS INTEGER), 0) AS sourceDuplicates, \
                COALESCE(CAST(timestamp AS TEXT),'') AS timestamp \
         FROM {table}{limit_clause}"
    );
    let rows = match sqlite_json_query(db_path, &sql) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let rel_id_raw = val_to_string(row.get("relationshipId"));
        let rel_id = if rel_id_raw == "0" { String::new() } else { rel_id_raw };
        out.push(AppleTriple {
            subject: val_to_string(row.get("subject")),
            predicate: val_to_string(row.get("predicate")),
            relationship_id: rel_id,
            relationship_predicate: val_to_string(row.get("relationshipPredicate")),
            object: val_to_string(row.get("object")),
            confidence: val_to_f64(row.get("confidence")),
            sources: val_to_string(row.get("sources")),
            source_duplicates: val_to_i64(row.get("sourceDuplicates")),
            timestamp: normalize_timestamp(&val_to_string(row.get("timestamp"))),
        });
    }
    Ok(out)
}

fn find_entity_tagging_db(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        dir.join("entityTagging.db"),
        dir.join("Artifacts").join("entityTagging").join("entityTagging.db"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn query_scorecards(dir: &Path) -> Result<Vec<AppleScorecard>> {
    let Some(tag_db) = find_entity_tagging_db(dir) else {
        return Ok(Vec::new());
    };
    let tables = sqlite_json_query(
        &tag_db,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    )
    .unwrap_or_default();
    let mut out = Vec::new();
    for tbl_row in tables {
        let tbl = val_to_string(tbl_row.get("name"));
        if tbl.is_empty() || tbl.starts_with("sqlite_") {
            continue;
        }
        let rows = match sqlite_json_query(&tag_db, &format!("SELECT * FROM \"{tbl}\"")) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let mut entity = String::new();
            for id_col in ["mdid", "entity", "entity_id", "entityId", "person_id", "personId", "subject", "id"] {
                let candidate = val_to_string(row.get(id_col));
                if !candidate.is_empty() {
                    entity = candidate;
                    break;
                }
            }
            if entity.is_empty() {
                continue;
            }
            let mut scores = BTreeMap::new();
            for col in KINSHIP_COLUMNS {
                let s = val_to_f64(row.get(col));
                if s > 0.0 {
                    scores.insert(col.to_string(), s);
                }
            }
            if !scores.is_empty() {
                out.push(AppleScorecard {
                    entity,
                    scores,
                    source: "apple/entityTagging.db".to_string(),
                });
            }
        }
    }
    Ok(out)
}

pub fn read_local_apple_store(
    dir: &Path,
    owner: &str,
    source: &str,
    limit: usize,
    entity_filter: Option<&str>,
) -> Result<LocalAppleSnapshot> {
    if !dir.is_dir() {
        bail!(
            "Apple IntelligencePlatform directory not found at {} (pass --dir or grant Full Disk Access on macOS)",
            dir.display()
        );
    }
    let graph_db = dir.join("graph.db");
    let (triples, expired) = if graph_db.is_file() {
        (
            query_graph_triples(&graph_db, "stable_graph", limit)?,
            query_graph_triples(&graph_db, "expired_stable_graph", limit)?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let scorecards = query_scorecards(dir)?;

    let mut entities_map: BTreeMap<String, LocalEntityView> = BTreeMap::new();
    let ensure_entity = |map: &mut BTreeMap<String, LocalEntityView>, id: &str| {
        map.entry(id.to_string()).or_insert_with(|| LocalEntityView {
            id: id.to_string(),
            class_id: String::new(),
            class_label: "entity".to_string(),
            name: String::new(),
            attributes: BTreeMap::new(),
            edges: Vec::new(),
            scorecard: BTreeMap::new(),
        });
    };

    #[derive(Default)]
    struct RelGroup {
        subject: String,
        predicate: String,
        rel_type: String,
        target: String,
        confidence: f64,
    }
    let mut rel_groups: BTreeMap<(String, String, String), RelGroup> = BTreeMap::new();

    for t in &triples {
        if t.subject.is_empty() || t.predicate.is_empty() {
            continue;
        }
        ensure_entity(&mut entities_map, &t.subject);
        if !t.relationship_id.is_empty() {
            let g = rel_groups
                .entry((t.subject.clone(), t.predicate.clone(), t.relationship_id.clone()))
                .or_insert_with(|| RelGroup {
                    subject: t.subject.clone(),
                    predicate: t.predicate.clone(),
                    confidence: t.confidence,
                    ..Default::default()
                });
            if t.relationship_predicate == "PS1" {
                g.rel_type = t.object.clone();
            } else if t.relationship_predicate == "PS45" || g.target.is_empty() {
                g.target = t.object.clone();
            }
            continue;
        }
        let ent = entities_map.get_mut(&t.subject).expect("entity exists");
        if t.predicate == "PS1" {
            ent.class_id = t.object.clone();
            ent.class_label = class_label(&t.object).to_string();
        } else if (t.predicate == "PS33" || t.predicate == "PS2") && ent.name.is_empty() {
            ent.name = t.object.clone();
        } else {
            let label = predicate_label(&t.predicate).to_string();
            let vals = ent.attributes.entry(label).or_default();
            if !vals.contains(&t.object) {
                vals.push(t.object.clone());
            }
        }
    }

    for sc in &scorecards {
        ensure_entity(&mut entities_map, &sc.entity);
        if let Some(ent) = entities_map.get_mut(&sc.entity) {
            if ent.class_id.is_empty() {
                ent.class_id = "SB104".to_string();
                ent.class_label = "person".to_string();
            }
            ent.scorecard = sc.scores.clone();
        }
    }

    let names_lookup: BTreeMap<String, String> = entities_map
        .iter()
        .filter(|(_, e)| !e.name.is_empty())
        .map(|(k, e)| (k.clone(), e.name.clone()))
        .collect();

    for (_, g) in rel_groups {
        if g.target.is_empty() {
            continue;
        }
        let target_name = names_lookup.get(&g.target).cloned().unwrap_or_default();
        if let Some(ent) = entities_map.get_mut(&g.subject) {
            ent.edges.push(LocalEdgeView {
                predicate_id: g.predicate.clone(),
                predicate_label: predicate_label(&g.predicate).to_string(),
                relationship_type_id: g.rel_type.clone(),
                relationship_type_label: class_label(&g.rel_type).to_string(),
                target_id: g.target,
                target_name,
                confidence: g.confidence,
            });
        }
    }

    let mut classes_breakdown: BTreeMap<String, usize> = BTreeMap::new();
    for ent in entities_map.values() {
        *classes_breakdown.entry(ent.class_label.clone()).or_default() += 1;
    }

    let mut entities: Vec<LocalEntityView> = entities_map
        .into_values()
        .filter(|e| {
            let Some(f) = entity_filter else { return true };
            let q = f.to_ascii_lowercase();
            e.id.to_ascii_lowercase().contains(&q)
                || e.name.to_ascii_lowercase().contains(&q)
                || e.class_label.to_ascii_lowercase().contains(&q)
        })
        .collect();

    entities.sort_by(|a, b| {
        let score_a = a.edges.len() * 3 + a.scorecard.len() * 2 + a.attributes.len() + usize::from(!a.name.is_empty());
        let score_b = b.edges.len() * 3 + b.scorecard.len() * 2 + b.attributes.len() + usize::from(!b.name.is_empty());
        score_b.cmp(&score_a).then_with(|| a.id.cmp(&b.id))
    });

    let now_rfc3339 = Utc::now().to_rfc3339();
    let apple_payload = json!({
        "owner": owner,
        "source": source,
        "at": now_rfc3339,
        "triples": triples,
        "expired": expired,
        "scorecards": scorecards,
    });

    Ok(LocalAppleSnapshot {
        dir: dir.display().to_string(),
        active_triples_total: triples.len(),
        expired_triples_total: expired.len(),
        scorecards_total: scorecards.len(),
        entities_total: entities.len(),
        classes_breakdown,
        entities,
        apple_payload,
    })
}

fn print_local_snapshot(snap: &LocalAppleSnapshot, max_entities: usize) {
    println!(
        "{} {}",
        "Apple Personal Knowledge Graph".cyan().bold(),
        format!("({})", snap.dir).dimmed()
    );
    println!(
        "  {} active triples: {}  ·  expired: {}  ·  kinship scorecards: {}  ·  entities: {}",
        "•".green(),
        snap.active_triples_total.to_string().bold(),
        snap.expired_triples_total.to_string().bold(),
        snap.scorecards_total.to_string().bold(),
        snap.entities_total.to_string().bold(),
    );
    if !snap.classes_breakdown.is_empty() {
        let summary: Vec<String> = snap
            .classes_breakdown
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        println!("  {} classes: {}", "•".cyan(), summary.join(", "));
    }
    println!();
    for ent in snap.entities.iter().take(max_entities) {
        let display_name = if ent.name.is_empty() {
            ent.id.clone()
        } else {
            format!("{} ({})", ent.name.bold(), ent.id.dimmed())
        };
        println!("  {} [{}]", display_name, ent.class_label.cyan());
        for (k, vals) in ent.attributes.iter().take(5) {
            let preview: Vec<&str> = vals.iter().map(String::as_str).take(3).collect();
            println!("      {}: {}", k.dimmed(), preview.join(", "));
        }
        for edge in ent.edges.iter().take(5) {
            let target = if edge.target_name.is_empty() {
                edge.target_id.clone()
            } else {
                format!("{} ({})", edge.target_name, edge.target_id)
            };
            println!(
                "      {} → {} [{}] (conf {:.2})",
                edge.predicate_label.yellow(),
                target,
                edge.relationship_type_label,
                edge.confidence
            );
        }
        if !ent.scorecard.is_empty() {
            let scores: Vec<String> = ent
                .scorecard
                .iter()
                .map(|(k, v)| format!("{k}={v:.2}"))
                .collect();
            println!("      {}: {}", "kinship scorecard".magenta(), scores.join(", "));
        }
    }
}

pub async fn run(cmd: GraphCommands, cfg: &mut Config) -> Result<()> {
    match cmd {
        GraphCommands::Local { dir, entity, limit, json } => {
            let path = match dir {
                Some(d) => d,
                None => default_intelligence_platform_dir()?,
            };
            let snap = read_local_apple_store(&path, "me", "apple/intelligenceplatform", limit, entity.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snap)?);
                return Ok(());
            }
            print_local_snapshot(&snap, 15);
            Ok(())
        }
        GraphCommands::Sync { dir, owner, source, limit, dry_run } => {
            let path = match dir {
                Some(d) => d,
                None => default_intelligence_platform_dir()?,
            };
            let snap = read_local_apple_store(&path, &owner, &source, limit, None)?;
            if dry_run {
                println!(
                    "{} Ready to sync {} active triples, {} expired rows, and {} kinship scorecards from {} to {}",
                    "✓".green().bold(),
                    snap.active_triples_total,
                    snap.expired_triples_total,
                    snap.scorecards_total,
                    snap.dir,
                    graph_route("apple"),
                );
                return Ok(());
            }
            crate::commands::product::call(
                cfg,
                Method::POST,
                graph_route("apple"),
                Some(snap.apple_payload),
                Vec::new(),
                false,
            )
            .await
        }
        GraphCommands::Ontology { group, q, local } => {
            if local {
                let payload = json!({
                    "classes_count": 181,
                    "predicates_count": 278,
                    "features_count": 144,
                    "scorecard_columns": KINSHIP_COLUMNS,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
                return Ok(());
            }
            let mut query = Vec::new();
            if let Some(g) = group {
                query.push(format!("group={g}"));
            }
            if let Some(q_str) = q {
                query.push(format!("q={q_str}"));
            }
            crate::commands::product::call(
                cfg,
                Method::GET,
                graph_route("ontology"),
                None,
                query,
                false,
            )
            .await
        }
        GraphCommands::Entity { entity, as_of, as_known, local, dir } => {
            if local {
                let path = match dir {
                    Some(d) => d,
                    None => default_intelligence_platform_dir()?,
                };
                let snap = read_local_apple_store(&path, "me", "apple/intelligenceplatform", 0, Some(&entity))?;
                print_local_snapshot(&snap, 10);
                return Ok(());
            }
            let mut body = serde_json::Map::new();
            body.insert("entity".to_string(), Value::String(entity));
            if let Some(t) = as_of {
                body.insert("as_of".to_string(), Value::String(t));
            }
            if let Some(t) = as_known {
                body.insert("as_known".to_string(), Value::String(t));
            }
            crate::commands::product::call(
                cfg,
                Method::POST,
                graph_route("entity"),
                Some(Value::Object(body)),
                Vec::new(),
                false,
            )
            .await
        }
        GraphCommands::Apple { dir, owner, source, at, limit, dry_run } => {
            let path = match dir {
                Some(d) => d,
                None => default_intelligence_platform_dir()?,
            };
            let mut snap = read_local_apple_store(&path, &owner, &source, limit, None)?;
            if let Some(at_str) = at {
                if let Some(obj) = snap.apple_payload.as_object_mut() {
                    obj.insert("at".to_string(), Value::String(at_str));
                }
            }
            if dry_run {
                println!("{}", serde_json::to_string_pretty(&snap.apple_payload)?);
                return Ok(());
            }
            crate::commands::product::call(
                cfg,
                Method::POST,
                graph_route("apple"),
                Some(snap.apple_payload),
                Vec::new(),
                false,
            )
            .await
        }
        GraphCommands::Convo { text, speaker, partner, source, at, dry_run } => {
            let now = at.unwrap_or_else(|| Utc::now().to_rfc3339());
            let mut turn = serde_json::Map::new();
            turn.insert("id".to_string(), Value::String("1".to_string()));
            turn.insert("speaker".to_string(), Value::String(speaker));
            if let Some(p) = partner {
                turn.insert("partner".to_string(), Value::String(p));
            }
            turn.insert("text".to_string(), Value::String(text));
            turn.insert("at".to_string(), Value::String(now.clone()));
            let payload = json!({
                "source": source,
                "at": now,
                "dry_run": dry_run,
                "turns": [Value::Object(turn)],
            });
            crate::commands::product::call(
                cfg,
                Method::POST,
                graph_route("convo"),
                Some(payload),
                Vec::new(),
                false,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_synthetic_apple_pkg_sqlite_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let graph_db = dir.join("graph.db");
        let tag_dir = dir.join("Artifacts").join("entityTagging");
        std::fs::create_dir_all(&tag_dir).expect("mkdir entityTagging");
        let tag_db = tag_dir.join("entityTagging.db");

        let status = Command::new("sqlite3")
            .arg(&graph_db)
            .arg(
                "CREATE TABLE stable_graph (subject INTEGER, predicate TEXT, relationshipId INTEGER, relationshipPredicate TEXT, object TEXT, sources INTEGER, confidence DOUBLE, sourceDuplicates TEXT, timestamp DOUBLE);
                 CREATE TABLE expired_stable_graph (subject INTEGER, predicate TEXT, relationshipId INTEGER, relationshipPredicate TEXT, object TEXT, sources INTEGER, confidence DOUBLE, sourceDuplicates TEXT, timestamp DOUBLE);
                 INSERT INTO stable_graph VALUES (9100000000007919, 'PS1', 0, '', 'SB104', 2, 1.0, '1', 809913600.0);
                 INSERT INTO stable_graph VALUES (9100000000007919, 'PS33', 0, '', 'Zach Kelling', 2, 1.0, '1', 809913600.0);
                 INSERT INTO stable_graph VALUES (9100000000013724, 'PS1', 0, '', 'SB10', 2, 1.0, '1', 809913600.0);
                 INSERT INTO stable_graph VALUES (9100000000013724, 'PS33', 0, '', 'Hanzo AI', 2, 1.0, '1', 809913600.0);
                 INSERT INTO stable_graph VALUES (9100000000007919, 'PS500', 4412, 'PS1', 'CS165', 2, 0.96, '1', 809913600.0);
                 INSERT INTO stable_graph VALUES (9100000000007919, 'PS500', 4412, 'PS45', '9100000000013724', 2, 0.96, '1', 809913600.0);
                 INSERT INTO expired_stable_graph VALUES (9100000000007919, 'PS33', 0, '', 'Old Name', 2, 0.5, '1', 809000000.0);",
            )
            .status()
            .expect("run sqlite3 graph.db");
        assert!(status.success());

        let status = Command::new("sqlite3")
            .arg(&tag_db)
            .arg(
                "CREATE TABLE entity_tagging (mdid TEXT PRIMARY KEY, mother DOUBLE, father DOUBLE, parent DOUBLE, sister DOUBLE, brother DOUBLE, sibling DOUBLE, family DOUBLE, friend DOUBLE, familyAndFriends DOUBLE, partner DOUBLE, coworker DOUBLE, alumni DOUBLE, child DOUBLE, son DOUBLE, daughter DOUBLE, myself DOUBLE, unknown DOUBLE, eventid INTEGER);
                 INSERT INTO entity_tagging VALUES ('9100000000007919', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0.99, 0, 1);",
            )
            .status()
            .expect("run sqlite3 entityTagging.db");
        assert!(status.success());

        let snap = read_local_apple_store(dir, "me", "apple/intelligenceplatform", 100, None)
            .expect("read_local_apple_store");
        assert_eq!(snap.active_triples_total, 6);
        assert_eq!(snap.expired_triples_total, 1);
        assert_eq!(snap.scorecards_total, 1);
        assert_eq!(snap.entities_total, 2);
        let me = snap.entities.iter().find(|e| e.id == "9100000000007919").expect("me entity");
        assert_eq!(me.name, "Zach Kelling");
        assert_eq!(me.class_label, "person");
        assert_eq!(me.edges.len(), 1);
        assert_eq!(me.edges[0].predicate_label, "employer");
        assert_eq!(me.edges[0].target_name, "Hanzo AI");
        assert_eq!(me.scorecard.get("myself").copied(), Some(0.99));
    }
}
