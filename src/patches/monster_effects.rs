//! Per-monster "keep original visuals" support for the `Effects` patch:
//! monsters are mapped to their effect files via the game's data tables and
//! the references embedded in monster metadata; a monster set to `Full` has
//! those files excluded from the Effects patch.

use super::datc64::{parse_table, row_strings, utf16le_string_at, DatTable};
use super::effect_skills::{clean_display_name, EffectLevel};
use super::targeting::{ends_with_path_ci, normalize_path, starts_with_path_ci};
use crate::bundle::{BundleFile, BundleIndex, BundleStore};
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

pub const MONSTER_VARIETIES_DATC64_PATH: &str = "data/balance/monstervarieties.datc64";
/// Secondary tables pairing monster paths with effect paths. All optional:
/// a missing table degrades the mapping, never fails it.
const MONSTER_EXTRA_TABLE_PATHS: &[&str] = &[
    "data/balance/monstervarietiesartvariations.datc64",
    "data/balance/legionmonstervarieties.datc64",
    "data/balance/miscanimated.datc64",
    "data/balance/itemvisualeffect.datc64",
];

const MONSTERS_PREFIX: &str = "metadata/monsters/";
const MONSTERS_EFFECTS_PREFIX: &str = "metadata/effects/spells/monsters_effects/";
const MIN_MONSTER_ALIAS_LEN: usize = 6;

/// Helper-asset subtrees under `metadata/monsters/` (attachment rigs, arena
/// markers) are not monsters. Folder spellings verified against a real
/// install: `attachments`, `effects`, and `objects`/`boss_objects`/
/// `arenaobjects`/`bossarenaobjects`/`orionarenaobjects` — hence the
/// ends-with match. Expects a normalized path; only folder segments count.
fn in_non_monster_subtree(path: &str) -> bool {
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    dir.split('/')
        .any(|segment| matches!(segment, "attachments" | "effects") || segment.ends_with("objects"))
}

static EFFECT_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)metadata[/\\]effects[/\\][^"'\s\)\};,]+"#).unwrap());

/// One persisted non-default per-monster setting. `monster` is the lowercase
/// metadata path without extension ("metadata/monsters/a/b/b"); monsters
/// without an override are `Reduced`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonsterEffectOverride {
    pub monster: String,
    pub level: EffectLevel,
}

/// One editor row: a display name grouping the monster variants that share
/// it within one family folder, plus hand-verified search aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonsterEffectCatalogEntry {
    pub display: String,
    pub monster_keys: Vec<String>,
    pub family: String,
    pub search_aliases: Vec<String>,
}

/// Hand-verified extra search terms for monsters players know by another
/// name; folded into the row's search text, never shown as the name.
/// The Abyssal Lich is "Vessel of Kulemak" in `monstervarieties` but known
/// by its lineage titles (Amanamu/Ulaman/Kurgal) and the "Amanamu's Void"
/// buff (`abyss_lightless_well` in `buffdefinitions`/`buffvisuals`).
const MONSTER_SEARCH_ALIASES: &[(&str, &[&str])] = &[(
    "metadata/monsters/leagueabyss/lichboss/kulemakboss",
    &["amanamu", "ulaman", "kurgal", "abyssal lich", "liege of the void"],
)];

fn search_aliases_for(monster_keys: &[String]) -> Vec<String> {
    MONSTER_SEARCH_ALIASES
        .iter()
        .filter(|(key, _)| monster_keys.iter().any(|k| k == key))
        .flat_map(|(_, aliases)| aliases.iter().map(|alias| alias.to_string()))
        .collect()
}

/// Effect paths mapped to one monster. `high`/`low` are patch targets split
/// by evidence confidence (game tables + metadata refs vs folder-name match
/// only). `refs` are non-target scannable files (`.epk`, out-of-scope `.ao`)
/// — never kept themselves, but the reference closure walks through them
/// because they can attach patch targets.
#[derive(Debug, Default)]
struct MonsterEdges {
    high: BTreeSet<String>,
    low: BTreeSet<String>,
    refs: BTreeSet<String>,
}

type EdgeMap = BTreeMap<String, MonsterEdges>;

/// Build the editor catalog: monsters with at least one high-confidence edge
/// leading (directly or through referenced effect files) to a patch target.
/// Heavy — GUI background thread only, never part of apply.
pub fn build_monster_effect_catalog(
    store: &BundleStore,
    index: &mut BundleIndex,
) -> Result<Vec<MonsterEffectCatalogEntry>> {
    index.ensure_paths_built()?;
    let mut candidates: Vec<(String, BundleFile)> = Vec::new();
    for path in index.paths() {
        if let Some(key) = monster_key_of(path) {
            if let Some(file) = index.file_by_path(path).copied() {
                candidates.push((key, file));
            }
        }
    }
    let monster_file_bytes = read_candidate_bytes(store, index, candidates)?;
    let monstervarieties = store.read_file(index, MONSTER_VARIETIES_DATC64_PATH).ok();
    let extra_tables: Vec<Vec<u8>> = MONSTER_EXTRA_TABLE_PATHS
        .iter()
        .filter_map(|path| store.read_file(index, path).ok())
        .collect();

    let edges = collect_edges(
        &monster_file_bytes,
        monstervarieties.as_deref(),
        &extra_tables,
    );
    // Monsters whose only refs are non-targets (.epk packs) still belong in
    // the catalog when those files transitively attach patch targets.
    let seeds: BTreeSet<String> = edges
        .values()
        .flat_map(|edge| edge.refs.iter().cloned())
        .collect();
    let graph = collect_effect_ref_graph(store, index, &seeds)?;
    let reaching_refs = paths_reaching_targets(&graph);

    Ok(catalog_rows(
        &edges,
        monstervarieties.as_deref(),
        &reaching_refs,
    ))
}

/// Resolve `Full` overrides to the patch-target paths that must stay
/// original. Stale monster keys resolve to nothing. Unlike the catalog,
/// low-confidence edges are included: keeping an unrelated extra file
/// original is visually safe, missing one of the monster's files is not.
pub(super) fn resolve_full_monster_effect_paths(
    store: &BundleStore,
    index: &mut BundleIndex,
    overrides: &[MonsterEffectOverride],
) -> Result<BTreeSet<String>> {
    let full_keys: BTreeSet<String> = overrides
        .iter()
        .filter(|entry| entry.level == EffectLevel::Full)
        .map(|entry| normalize_path(&entry.monster))
        .collect();
    if full_keys.is_empty() {
        return Ok(BTreeSet::new());
    }

    index.ensure_paths_built()?;
    // Alias uniqueness needs the full monster-key population.
    let mut all_keys: BTreeSet<String> = BTreeSet::new();
    let mut candidates: Vec<(String, BundleFile)> = Vec::new();
    for path in index.paths() {
        if let Some(key) = monster_key_of(path) {
            if full_keys.contains(&key) {
                if let Some(file) = index.file_by_path(path).copied() {
                    candidates.push((key.clone(), file));
                }
            }
            all_keys.insert(key);
        }
    }

    let mut edges = EdgeMap::new();
    for (key, bytes) in read_candidate_bytes(store, index, candidates)? {
        collect_metadata_ref_edges(&key, &bytes, &mut edges);
    }
    if let Ok(bytes) = store.read_file(index, MONSTER_VARIETIES_DATC64_PATH) {
        collect_table_edges(&bytes, &full_keys, &mut edges);
    }
    for path in MONSTER_EXTRA_TABLE_PATHS {
        if let Ok(bytes) = store.read_file(index, path) {
            collect_table_edges(&bytes, &full_keys, &mut edges);
        }
    }
    let aliases = unique_monster_aliases(&all_keys);
    collect_folder_alias_edges(index.paths(), &aliases, &full_keys, &mut edges);

    let mut kept: BTreeSet<String> = BTreeSet::new();
    let mut seeds: BTreeSet<String> = BTreeSet::new();
    for edge in edges.into_values() {
        for path in edge.high.into_iter().chain(edge.low) {
            seeds.insert(path.clone());
            kept.insert(path);
        }
        seeds.extend(edge.refs);
    }
    // Kept files can attach further effect files (.ao -> .ao, .epk -> .ao);
    // walk those so a kept visual's children don't render half-stripped.
    let graph = collect_effect_ref_graph(store, index, &seeds)?;
    for refs in graph.values() {
        for path in refs {
            if is_effects_patch_target(path) {
                kept.insert(path.clone());
            }
        }
    }
    Ok(kept)
}

fn read_candidate_bytes(
    store: &BundleStore,
    index: &BundleIndex,
    candidates: Vec<(String, BundleFile)>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let files: Vec<BundleFile> = candidates.iter().map(|(_, file)| *file).collect();
    let mut by_hash = store
        .read_files_batch(index, &files)
        .context("read monster metadata files for per-monster effects")?;
    Ok(candidates
        .into_iter()
        .filter_map(|(key, file)| by_hash.remove(&file.hash).map(|bytes| (key, bytes)))
        .collect())
}

/// Pure edge collection over pre-read data; keys repeat in
/// `monster_file_bytes` when a monster has several metadata files.
fn collect_edges(
    monster_file_bytes: &[(String, Vec<u8>)],
    monstervarieties: Option<&[u8]>,
    extra_tables: &[Vec<u8>],
) -> EdgeMap {
    let all_keys: BTreeSet<String> = monster_file_bytes
        .iter()
        .map(|(key, _)| key.clone())
        .collect();

    let mut edges = EdgeMap::new();
    for (key, bytes) in monster_file_bytes {
        collect_metadata_ref_edges(key, bytes, &mut edges);
    }
    if let Some(bytes) = monstervarieties {
        collect_table_edges(bytes, &all_keys, &mut edges);
    }
    for bytes in extra_tables {
        collect_table_edges(bytes, &all_keys, &mut edges);
    }
    edges
}

/// Pure catalog assembly. Rows group variants sharing a display name *within
/// one family folder* — display text alone couples unrelated monsters
/// (dozens of distinct monsters are all named "Daemon").
fn catalog_rows(
    edges: &EdgeMap,
    monstervarieties: Option<&[u8]>,
    reaching_refs: &BTreeSet<String>,
) -> Vec<MonsterEffectCatalogEntry> {
    let displays = monstervarieties
        .map(monster_display_names)
        .unwrap_or_default();

    let mut by_group: BTreeMap<(String, String), MonsterEffectCatalogEntry> = BTreeMap::new();
    for (key, edge) in edges {
        let listed = !edge.high.is_empty()
            || edge.refs.iter().any(|path| reaching_refs.contains(path));
        if !listed {
            continue;
        }
        let display = displays
            .get(key.as_str())
            .cloned()
            .unwrap_or_else(|| humanize_key(key));
        let row = by_group
            .entry((display.to_lowercase(), family_of(key)))
            .or_insert_with(|| MonsterEffectCatalogEntry {
                display,
                monster_keys: Vec::new(),
                family: family_of(key),
                search_aliases: Vec::new(),
            });
        row.monster_keys.push(key.clone());
    }

    let mut rows: Vec<MonsterEffectCatalogEntry> = by_group.into_values().collect();
    for row in &mut rows {
        row.monster_keys.sort();
        row.monster_keys.dedup();
        row.search_aliases = search_aliases_for(&row.monster_keys);
    }
    rows.sort_by(|a, b| {
        a.display
            .to_lowercase()
            .cmp(&b.display.to_lowercase())
            .then_with(|| a.display.cmp(&b.display))
            .then_with(|| a.family.cmp(&b.family))
    });
    rows
}

/// The lowercase monster key for a monster metadata index path, or `None`
/// for paths that are not monster metadata.
fn monster_key_of(path: &str) -> Option<String> {
    if !starts_with_path_ci(path, MONSTERS_PREFIX) {
        return None;
    }
    if !(ends_with_path_ci(path, ".ot")
        || ends_with_path_ci(path, ".ao")
        || ends_with_path_ci(path, ".act"))
    {
        return None;
    }
    let normalized = normalize_path(path);
    if in_non_monster_subtree(&normalized) {
        return None;
    }
    Some(strip_ext(&normalized).to_string())
}

/// The monster key for a path found in a data-table row. `monstervarieties`
/// stores metadata paths *without* an extension (verified in-game data), so
/// extensionless paths are the key itself; other extensions (.ais etc.) are
/// not monster identity. Phantom keys from row noise are harmless: callers
/// filter against the real key population.
fn monster_key_from_table_path(path: &str) -> Option<String> {
    if !path.starts_with(MONSTERS_PREFIX) || in_non_monster_subtree(path) {
        return None;
    }
    match path.rsplit('/').next()?.rsplit_once('.') {
        None => Some(path.to_string()),
        Some((_, "ot" | "ao" | "act")) => Some(strip_ext(path).to_string()),
        Some(_) => None,
    }
}

/// Table rows pairing a known monster path with effect paths yield
/// high-confidence edges.
fn collect_table_edges(bytes: &[u8], monster_keys: &BTreeSet<String>, edges: &mut EdgeMap) {
    let Some(table) = parse_table(bytes) else {
        return;
    };
    for row in table.rows() {
        let strings = row_strings(row, table.heap());
        let row_keys: Vec<String> = strings
            .iter()
            .filter_map(|value| normalized_metadata_path(value))
            .filter_map(|path| monster_key_from_table_path(&path))
            .filter(|key| monster_keys.contains(key))
            .collect();
        if row_keys.is_empty() {
            continue;
        }
        let mut target_paths: Vec<String> = Vec::new();
        let mut ref_paths: Vec<String> = Vec::new();
        for path in strings.iter().filter_map(|value| normalized_metadata_path(value)) {
            if is_effects_patch_target(&path) {
                target_paths.push(path);
            } else if is_scannable_effect_file(&path) {
                ref_paths.push(path);
            }
        }
        for key in row_keys {
            let edge = edges.entry(key).or_default();
            edge.high.extend(target_paths.iter().cloned());
            edge.refs.extend(ref_paths.iter().cloned());
        }
    }
}

/// Effect paths referenced by a monster's own metadata file: targets become
/// high-confidence edges, non-target scannable files become closure seeds.
fn collect_metadata_ref_edges(monster_key: &str, bytes: &[u8], edges: &mut EdgeMap) {
    let text = decode_text_lossy(bytes);
    for matched in EFFECT_REF_RE.find_iter(&text) {
        let Some(path) = normalized_metadata_path(matched.as_str()) else {
            continue;
        };
        if is_effects_patch_target(&path) {
            edges
                .entry(monster_key.to_string())
                .or_default()
                .high
                .insert(path);
        } else if is_scannable_effect_file(&path) {
            edges
                .entry(monster_key.to_string())
                .or_default()
                .refs
                .insert(path);
        }
    }
}

const REF_CLOSURE_MAX_FILES: usize = 8192;
const REF_CLOSURE_MAX_DEPTH: usize = 8;

/// Effect files that can attach further effect files and are worth reading
/// during the reference closure; expects a normalized path.
fn is_scannable_effect_file(path: &str) -> bool {
    path.starts_with("metadata/effects/") && (is_target_ext(path) || path.ends_with(".epk"))
}

/// Breadth-first reference closure over scannable effect files. Returns
/// path -> referenced paths for every file read; missing seeds have no node.
/// The depth/file caps cut runaway graphs short — a truncated closure only
/// means some deeply nested children stay reduced, never an error.
fn collect_effect_ref_graph(
    store: &BundleStore,
    index: &BundleIndex,
    seeds: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<String> = seeds
        .iter()
        .filter(|path| is_scannable_effect_file(path))
        .cloned()
        .collect();
    for _ in 0..REF_CLOSURE_MAX_DEPTH {
        let mut candidates: Vec<(String, BundleFile)> = Vec::new();
        for path in frontier.drain(..) {
            if visited.len() >= REF_CLOSURE_MAX_FILES {
                break;
            }
            if !visited.insert(path.clone()) {
                continue;
            }
            if let Some(file) = index.file_by_path(&path).copied() {
                candidates.push((path, file));
            }
        }
        if candidates.is_empty() {
            break;
        }
        for (path, bytes) in read_candidate_bytes(store, index, candidates)? {
            let refs = scannable_effect_refs(&bytes);
            frontier.extend(refs.iter().cloned());
            graph.insert(path, refs);
        }
    }
    Ok(graph)
}

fn scannable_effect_refs(bytes: &[u8]) -> Vec<String> {
    let text = decode_text_lossy(bytes);
    let mut refs: Vec<String> = Vec::new();
    for matched in EFFECT_REF_RE.find_iter(&text) {
        if let Some(path) = normalized_metadata_path(matched.as_str()) {
            if is_scannable_effect_file(&path) && !refs.contains(&path) {
                refs.push(path);
            }
        }
    }
    refs
}

/// Graph nodes leading to at least one patch target, directly or through
/// other files. A referenced target counts even when the target file itself
/// was missing or cut off by the closure caps.
fn paths_reaching_targets(graph: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut reaches: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut changed = false;
        for (path, refs) in graph {
            if reaches.contains(path) {
                continue;
            }
            if is_effects_patch_target(path)
                || refs
                    .iter()
                    .any(|r| is_effects_patch_target(r) || reaches.contains(r))
            {
                reaches.insert(path.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    reaches
}

/// `monsters_effects` folders whose name unambiguously matches one monster
/// alias yield low-confidence edges.
fn collect_folder_alias_edges(
    paths: &[String],
    aliases: &HashMap<String, String>,
    wanted_keys: &BTreeSet<String>,
    edges: &mut EdgeMap,
) {
    for path in paths {
        if !starts_with_path_ci(path, MONSTERS_EFFECTS_PREFIX) || !is_target_ext(path) {
            continue;
        }
        let normalized = normalize_path(path);
        let key_part = strip_ext(
            normalized
                .strip_prefix(MONSTERS_EFFECTS_PREFIX)
                .unwrap_or(&normalized),
        );
        let Some(monster_key) = key_part
            .split('/')
            .rev()
            .map(normalize_key)
            .filter(|alias| alias.len() >= MIN_MONSTER_ALIAS_LEN)
            .find_map(|alias| aliases.get(&alias))
        else {
            continue;
        };
        if !wanted_keys.contains(monster_key) {
            continue;
        }
        edges
            .entry(monster_key.clone())
            .or_default()
            .low
            .insert(normalized);
    }
}

/// Alias -> monster key for every path-segment alias that identifies exactly
/// one monster; ambiguous aliases are dropped.
fn unique_monster_aliases(monster_keys: &BTreeSet<String>) -> HashMap<String, String> {
    let mut aliases: HashMap<String, Option<&String>> = HashMap::new();
    for monster_key in monster_keys {
        // skip(2): "metadata" and "monsters".
        for raw_alias in monster_key.split('/').skip(2) {
            let alias = normalize_key(raw_alias);
            if alias.len() < MIN_MONSTER_ALIAS_LEN {
                continue;
            }
            aliases
                .entry(alias)
                .and_modify(|existing| {
                    if *existing != Some(monster_key) {
                        *existing = None;
                    }
                })
                .or_insert(Some(monster_key));
        }
    }
    aliases
        .into_iter()
        .filter_map(|(alias, monster_key)| monster_key.map(|key| (alias, key.clone())))
        .collect()
}

/// Monster key -> display name from `monstervarieties` rows. The schema is
/// unknown, so the name column is detected statistically (see
/// `detect_display_column`); a naive first-plausible-string scan grouped 237
/// unrelated monsters under "Any" on a real install.
fn monster_display_names(bytes: &[u8]) -> HashMap<String, String> {
    let mut displays = HashMap::new();
    let Some(table) = parse_table(bytes) else {
        return displays;
    };
    let Some(column) = detect_display_column(&table) else {
        return displays;
    };
    for row in table.rows() {
        let Some(display) = string_at_column(row, table.heap(), column).and_then(|value| {
            monster_display(&value)
        }) else {
            continue;
        };
        let keys = row_strings(row, table.heap())
            .iter()
            .filter_map(|value| normalized_metadata_path(value))
            .filter_map(|path| monster_key_from_table_path(&path))
            .collect::<Vec<_>>();
        for key in keys {
            displays.entry(key).or_insert_with(|| display.clone());
        }
    }
    displays
}

/// A field position within a fixed-size row: byte offset plus whether the
/// heap offset is u64 (`wide`) or u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FieldColumn {
    offset: usize,
    wide: bool,
}

const DISPLAY_COLUMN_SAMPLE_ROWS: usize = 512;

/// Score every candidate (offset, width) by how many sampled rows yield a
/// multi-word name there. Multi-word only for scoring — shared single-word
/// fields ("Any") must never win; extraction then accepts single words too.
fn detect_display_column(table: &DatTable<'_>) -> Option<FieldColumn> {
    let heap = table.heap();
    let mut scores: HashMap<(usize, bool), usize> = HashMap::new();
    for row in table.rows().take(DISPLAY_COLUMN_SAMPLE_ROWS) {
        for offset in (0..row.len()).step_by(4) {
            for wide in [true, false] {
                let column = FieldColumn { offset, wide };
                if let Some(value) = string_at_column(row, heap, column) {
                    if monster_display(&value).is_some_and(|name| name.contains(' ')) {
                        *scores.entry((offset, wide)).or_default() += 1;
                    }
                }
            }
        }
    }
    // Ties break toward the lowest offset, u32 first.
    scores
        .into_iter()
        .max_by_key(|((offset, wide), score)| (*score, usize::MAX - offset, !wide))
        .map(|((offset, wide), _)| FieldColumn { offset, wide })
}

fn string_at_column(row: &[u8], heap: &[u8], column: FieldColumn) -> Option<String> {
    let width = if column.wide { 8 } else { 4 };
    let field = row.get(column.offset..column.offset + width)?;
    let offset = if column.wide {
        u64::from_le_bytes(field.try_into().ok()?) as usize
    } else {
        u32::from_le_bytes(field.try_into().ok()?) as usize
    };
    utf16le_string_at(heap, offset)
}

/// A plausible in-game monster name: multi-word text ("Bog Hulk") or a
/// single capitalized word ("Hyena"). Rejects paths, CamelCase internal ids,
/// all-caps markers, and bracket fragments.
fn monster_display(value: &str) -> Option<String> {
    if value.contains('/') || value.contains('\\') || value.contains('.') {
        return None;
    }
    let cleaned = clean_display_name(value)?;
    let cleaned = cleaned.trim();
    if cleaned.is_empty()
        || cleaned.contains('[')
        || cleaned.contains(']')
        || !cleaned.chars().any(|c| c.is_ascii_lowercase())
    {
        return None;
    }
    let plausible = cleaned.contains(' ') || is_single_word_name(cleaned);
    plausible.then(|| cleaned.to_string())
}

fn is_single_word_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_lowercase() || c == '\'' || c == '-')
}

/// Extract and normalize an embedded `metadata/...` path from a raw string;
/// normalizes first so backslash spellings resolve, then trims quote noise.
fn normalized_metadata_path(value: &str) -> Option<String> {
    let normalized = normalize_path(value);
    let start = normalized.find("metadata/")?;
    let path = normalized[start..].trim_matches(|ch: char| {
        matches!(ch, '"' | '\'' | ')' | '}' | ';' | ',' | '\r' | '\n' | ' ' | '\t')
    });
    (!path.is_empty()).then(|| path.to_string())
}

/// Matches the Effects patch's long-standing target shape (`.ao`/`.aoc`
/// under `metadata/effects/spells/`); expects a normalized path.
fn is_effects_patch_target(path: &str) -> bool {
    path.starts_with("metadata/effects/spells/") && is_target_ext(path)
}

fn is_target_ext(path: &str) -> bool {
    ends_with_path_ci(path, ".ao") || ends_with_path_ci(path, ".aoc")
}

fn strip_ext(path: &str) -> &str {
    path.rsplit_once('.').map_or(path, |(base, _)| base)
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn family_of(monster_key: &str) -> String {
    monster_key
        .split('/')
        .nth(2)
        .unwrap_or_default()
        .to_string()
}

fn humanize_key(monster_key: &str) -> String {
    monster_key
        .rsplit('/')
        .next()
        .unwrap_or(monster_key)
        .replace('_', " ")
}

fn decode_text_lossy(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Test-only shorthand: edges + assembly in one step, with no reference
/// closure (an empty reachability set).
#[cfg(test)]
fn monster_effect_catalog_from_parts(
    monster_file_bytes: &[(String, Vec<u8>)],
    monstervarieties: Option<&[u8]>,
    extra_tables: &[Vec<u8>],
) -> Vec<MonsterEffectCatalogEntry> {
    let edges = collect_edges(monster_file_bytes, monstervarieties, extra_tables);
    catalog_rows(&edges, monstervarieties, &BTreeSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ROW_MARKER: [u8; 8] = [0xbb; 8];

    fn push_utf16(heap: &mut Vec<u8>, s: &str) -> u64 {
        let offset = heap.len() as u64;
        for unit in s.encode_utf16() {
            heap.extend_from_slice(&unit.to_le_bytes());
        }
        heap.extend_from_slice(&0u16.to_le_bytes());
        offset
    }

    fn table(rows: &[&[&str]]) -> Vec<u8> {
        let mut heap = TEST_ROW_MARKER.to_vec();
        let offsets: Vec<Vec<u64>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|value| push_utf16(&mut heap, value))
                    .collect()
            })
            .collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for row in offsets {
            for offset in row {
                bytes.extend_from_slice(&offset.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&heap);
        bytes
    }

    fn utf16_file(text: &str) -> Vec<u8> {
        let mut out = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    #[test]
    fn monster_key_extraction_normalizes_and_filters() {
        for (path, expected) in [
            (
                "Metadata/Monsters/BogHulk/BogHulk.ot",
                Some("metadata/monsters/boghulk/boghulk"),
            ),
            (
                "metadata\\monsters\\bird\\mutantbird.ao",
                Some("metadata/monsters/bird/mutantbird"),
            ),
            (
                "metadata/monsters/chimera/chimera.act",
                Some("metadata/monsters/chimera/chimera"),
            ),
            // Helper subtrees are not monsters (all seen on a real install:
            // attachment rigs, arena markers, standalone effect helpers).
            ("metadata/monsters/boghulk/attachments/arm.ao", None),
            ("metadata/monsters/boghulk/objects/rock.ot", None),
            ("metadata/monsters/mastodonboss/effects/mastodonspikes.ao", None),
            ("metadata/monsters/hyenamonster/effects/ao/footstep.ao", None),
            ("metadata/monsters/balbala/arenaobjects/clone_rune.ot", None),
            ("metadata/monsters/forsakenson/bossarenaobjects/flamewall.ot", None),
            ("metadata/monsters/owlboss/boss_objects/snowball.ot", None),
            // A file merely *named* like a helper folder is still a monster.
            (
                "metadata/monsters/golem/stoneobjects.ot",
                Some("metadata/monsters/golem/stoneobjects"),
            ),
            // Wrong extension / wrong prefix.
            ("metadata/monsters/boghulk/boghulk.otc", None),
            ("metadata/monsters/boghulk/boghulk.aoc", None),
            ("metadata/effects/spells/fireball/fireball.ao", None),
        ] {
            assert_eq!(monster_key_of(path).as_deref(), expected, "path: {path}");
        }
    }

    #[test]
    fn metadata_ref_edges_split_targets_and_scannable_refs() {
        let text = r#"version 3
extends "Metadata/Monsters/Monster"
on_spawn = "AddEffectPack( Metadata/Effects/Spells/monsters_effects/act1/boghulk/spawn.epk );"
rig = "Metadata/Effects/Spells/monsters_effects/act1/boghulk/slam.ao"
aux = "Metadata\Effects\Spells\NPC\aoife\ghost.aoc"
particle = "Metadata/Effects/Spells/monsters_effects/act1/boghulk/fx/dust.pet"
other = "Metadata/Effects/utility/epks/off.ao"
"#;
        let mut edges = EdgeMap::new();

        collect_metadata_ref_edges("metadata/monsters/boghulk/boghulk", &utf16_file(text), &mut edges);
        // UTF-8 (no BOM) files scan the same way.
        collect_metadata_ref_edges("metadata/monsters/boghulk/boghulk", text.as_bytes(), &mut edges);

        let edge = &edges["metadata/monsters/boghulk/boghulk"];
        assert_eq!(
            edge.high.iter().cloned().collect::<Vec<_>>(),
            vec![
                "metadata/effects/spells/monsters_effects/act1/boghulk/slam.ao".to_string(),
                "metadata/effects/spells/npc/aoife/ghost.aoc".to_string(),
            ]
        );
        assert!(edge.low.is_empty());
        // Non-target scannable files become closure seeds; leaf assets
        // (.pet) never do.
        assert_eq!(
            edge.refs.iter().cloned().collect::<Vec<_>>(),
            vec![
                "metadata/effects/spells/monsters_effects/act1/boghulk/spawn.epk".to_string(),
                "metadata/effects/utility/epks/off.ao".to_string(),
            ]
        );
    }

    #[test]
    fn table_edges_require_monster_and_effect_in_same_row() {
        let keys: BTreeSet<String> =
            [("metadata/monsters/boghulk/boghulk".to_string())].into();
        let bytes = table(&[
            &[
                "Metadata/Monsters/BogHulk/BogHulk.ot",
                "Metadata/Effects/Spells/monsters_effects/boghulk/slam.ao",
                "Metadata/Effects/Spells/monsters_effects/boghulk/spawn.epk",
            ],
            &[
                "Metadata/Monsters/Other/Other.ot",
                "Metadata/Effects/Spells/monsters_effects/other/roar.ao",
            ],
        ]);
        let mut edges = EdgeMap::new();

        collect_table_edges(&bytes, &keys, &mut edges);

        assert_eq!(edges.len(), 1);
        let edge = &edges["metadata/monsters/boghulk/boghulk"];
        // Only the .ao is a patch target; the .epk becomes a closure seed.
        assert_eq!(
            edge.high.iter().cloned().collect::<Vec<_>>(),
            vec!["metadata/effects/spells/monsters_effects/boghulk/slam.ao".to_string()]
        );
        assert_eq!(
            edge.refs.iter().cloned().collect::<Vec<_>>(),
            vec!["metadata/effects/spells/monsters_effects/boghulk/spawn.epk".to_string()]
        );
    }

    #[test]
    fn folder_alias_edges_match_unique_aliases_only() {
        let all_keys: BTreeSet<String> = [
            "metadata/monsters/anchorman/anchormangreen".to_string(),
            // "shared" appears under two different monsters -> ambiguous.
            "metadata/monsters/familya/shared".to_string(),
            "metadata/monsters/familyb/shared".to_string(),
            "metadata/monsters/act/tiny".to_string(),
        ]
        .into();
        let aliases = unique_monster_aliases(&all_keys);
        assert_eq!(
            aliases.get("anchormangreen").map(String::as_str),
            Some("metadata/monsters/anchorman/anchormangreen")
        );
        assert!(!aliases.contains_key("shared"));
        // Short segments never become aliases.
        assert!(!aliases.contains_key("tiny"));
        assert!(!aliases.contains_key("act"));

        let paths: Vec<String> = [
            "Metadata/Effects/Spells/monsters_effects/act4/anchormangreen/death.ao",
            "metadata/effects/spells/monsters_effects/act4/anchormangreen/death.pet",
            "metadata/effects/spells/monsters_effects/act1/shared/roar.ao",
            "metadata/effects/spells/fireball/fireball.ao",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let wanted: BTreeSet<String> =
            ["metadata/monsters/anchorman/anchormangreen".to_string()].into();
        let mut edges = EdgeMap::new();

        collect_folder_alias_edges(&paths, &aliases, &wanted, &mut edges);

        assert_eq!(edges.len(), 1);
        let edge = &edges["metadata/monsters/anchorman/anchormangreen"];
        assert_eq!(
            edge.low.iter().cloned().collect::<Vec<_>>(),
            vec![
                "metadata/effects/spells/monsters_effects/act4/anchormangreen/death.ao"
                    .to_string()
            ]
        );
    }

    #[test]
    fn monster_display_accepts_names_and_rejects_ids() {
        assert_eq!(monster_display("Bog Hulk").as_deref(), Some("Bog Hulk"));
        assert_eq!(monster_display("Hyena").as_deref(), Some("Hyena"));
        assert_eq!(monster_display("Za'atash").as_deref(), Some("Za'atash"));
        assert_eq!(
            monster_display("[DNT] Bog Hulk").as_deref(),
            Some("Bog Hulk")
        );
        // CamelCase internal ids, paths, markers, lowercase ids.
        assert_eq!(monster_display("SummonedSkeleton"), None);
        assert_eq!(monster_display("Metadata/Monsters/BogHulk/BogHulk.ot"), None);
        assert_eq!(monster_display("art\\models\\thing"), None);
        assert_eq!(monster_display("DISCONTINUED Bog Hulk"), None);
        assert_eq!(monster_display("bog_hulk"), None);
        assert_eq!(monster_display(""), None);
    }

    #[test]
    fn catalog_groups_variants_by_display_and_requires_high_edges() {
        let mother = "metadata/monsters/anchorite/anchoritemother/anchoritemother";
        let runemarked =
            "metadata/monsters/anchorite/anchoritemother/runemarked/anchoritemotherrunemarked";
        let petless = "metadata/monsters/bird/mutantbird";
        let monster_files: Vec<(String, Vec<u8>)> = vec![
            (
                mother.to_string(),
                utf16_file(
                    r#"rig = "Metadata/Effects/Spells/monsters_effects/act3/anchoritemother/idle.ao""#,
                ),
            ),
            (
                runemarked.to_string(),
                utf16_file(
                    r#"rig = "Metadata/Effects/Spells/monsters_effects/act3/anchoritemother/idle.ao""#,
                ),
            ),
            // Only references a .pet -> no patch-target edge -> not listed.
            (
                petless.to_string(),
                utf16_file(
                    r#"fx = "Metadata/Effects/Spells/monsters_effects/act1/bird/feathers.pet""#,
                ),
            ),
        ];
        let varieties = table(&[
            &["Metadata/Monsters/Anchorite/AnchoriteMother/AnchoriteMother.ot", "Filthy First-born"],
            &[
                "Metadata/Monsters/Anchorite/AnchoriteMother/Runemarked/AnchoriteMotherRunemarked.ot",
                "Filthy First-born",
            ],
            &["Metadata/Monsters/Bird/MutantBird.ao", "Mutant Bird"],
        ]);

        let rows = monster_effect_catalog_from_parts(&monster_files, Some(&varieties), &[]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display, "Filthy First-born");
        assert_eq!(rows[0].family, "anchorite");
        assert_eq!(
            rows[0].monster_keys,
            vec![mother.to_string(), runemarked.to_string()]
        );
    }

    #[test]
    fn display_names_come_from_the_detected_name_column_only() {
        // A generic "Any" column precedes the real name column in every row;
        // detection must skip it, and extensionless monster paths (the
        // monstervarieties spelling) must key the display map.
        let varieties = table(&[
            &["Metadata/Monsters/BogHulk/BogHulk", "Any", "Bog Hulk"],
            &["Metadata/Monsters/SandSpitter/SandSpitter", "Any", "Sand Spitter"],
            &["Metadata/Monsters/Unnamed/Unnamed", "Any", ""],
        ]);

        let displays = monster_display_names(&varieties);

        assert_eq!(
            displays
                .get("metadata/monsters/boghulk/boghulk")
                .map(String::as_str),
            Some("Bog Hulk")
        );
        assert_eq!(
            displays
                .get("metadata/monsters/sandspitter/sandspitter")
                .map(String::as_str),
            Some("Sand Spitter")
        );
        // No name in the detected column -> no display entry (humanized
        // fallback happens at catalog assembly), and "Any" never leaks in.
        assert!(!displays.contains_key("metadata/monsters/unnamed/unnamed"));
        assert!(displays.values().all(|display| display != "Any"));
    }

    #[test]
    fn catalog_falls_back_to_humanized_names_without_varieties_table() {
        let monster_files: Vec<(String, Vec<u8>)> = vec![(
            "metadata/monsters/bog_hulk/bog_hulk".to_string(),
            utf16_file(r#"rig = "Metadata/Effects/Spells/monsters_effects/act1/bog_hulk/slam.ao""#),
        )];

        let rows = monster_effect_catalog_from_parts(&monster_files, None, &[]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display, "bog hulk");
        assert_eq!(rows[0].family, "bog_hulk");
    }

    #[test]
    fn catalog_attaches_hand_verified_search_aliases() {
        let lich = "metadata/monsters/leagueabyss/lichboss/kulemakboss";
        let monster_files: Vec<(String, Vec<u8>)> = vec![
            (
                lich.to_string(),
                utf16_file(
                    r#"rig = "Metadata/Effects/Spells/monsters_effects/League_Abyss/Kulemak/shrink_ring.ao""#,
                ),
            ),
            (
                "metadata/monsters/boghulk/boghulk".to_string(),
                utf16_file(
                    r#"rig = "Metadata/Effects/Spells/monsters_effects/act1/boghulk/slam.ao""#,
                ),
            ),
        ];

        let rows = monster_effect_catalog_from_parts(&monster_files, None, &[]);

        let lich_row = rows
            .iter()
            .find(|row| row.monster_keys.iter().any(|key| key == lich))
            .unwrap();
        assert!(lich_row
            .search_aliases
            .iter()
            .any(|alias| alias == "amanamu"));
        let other = rows
            .iter()
            .find(|row| row.monster_keys[0].contains("boghulk"))
            .unwrap();
        assert!(other.search_aliases.is_empty());
    }

    #[test]
    fn catalog_uses_table_edges_when_metadata_has_no_refs() {
        let monster_files: Vec<(String, Vec<u8>)> = vec![(
            "metadata/monsters/boghulk/boghulk".to_string(),
            utf16_file("version 3"),
        )];
        let varieties = table(&[&[
            "Metadata/Monsters/BogHulk/BogHulk.ot",
            "Bog Hulk",
            "Metadata/Effects/Spells/monsters_effects/act1/boghulk/slam.ao",
        ]]);

        let rows = monster_effect_catalog_from_parts(&monster_files, Some(&varieties), &[]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display, "Bog Hulk");
    }

    #[test]
    fn scannable_effect_refs_extracts_normalized_scannable_paths() {
        let text = r#"version 2
AttachedObject "<root>" "Metadata/Effects/Spells/monsters_effects/act1/boghulk/child.aoc"
ParticleEffect "grp" "Metadata/Effects/Spells/monsters_effects/act1/boghulk/fx/dust.pet"
"filename": "Metadata/Effects/Spells/monsters_effects/act1/boghulk/mat/skin.mat"
epk = "Metadata\Effects\Utility\epks\pack.epk"
"#;
        for bytes in [utf16_file(text), text.as_bytes().to_vec()] {
            assert_eq!(
                scannable_effect_refs(&bytes),
                vec![
                    "metadata/effects/spells/monsters_effects/act1/boghulk/child.aoc".to_string(),
                    "metadata/effects/utility/epks/pack.epk".to_string(),
                ]
            );
        }
    }

    #[test]
    fn paths_reaching_targets_propagates_through_chains_and_cycles() {
        let target = "metadata/effects/spells/monsters_effects/x/child.aoc".to_string();
        let mid = "metadata/effects/utility/epks/pack.epk".to_string();
        let root = "metadata/effects/utility/epks/root.epk".to_string();
        let dead = "metadata/effects/utility/epks/dead.epk".to_string();
        // Mutually referencing files must not loop forever and must not
        // count as reaching when neither leads to a target.
        let loop_a = "metadata/effects/utility/epks/loop_a.epk".to_string();
        let loop_b = "metadata/effects/utility/epks/loop_b.epk".to_string();
        let graph: BTreeMap<String, Vec<String>> = [
            // root -> mid -> target, where the target file itself was never
            // read (missing from the graph): the reference alone counts.
            (root.clone(), vec![mid.clone()]),
            (mid.clone(), vec![target.clone()]),
            (dead.clone(), Vec::new()),
            (loop_a.clone(), vec![loop_b.clone()]),
            (loop_b.clone(), vec![loop_a.clone()]),
        ]
        .into();

        let reaches = paths_reaching_targets(&graph);

        assert!(reaches.contains(&root));
        assert!(reaches.contains(&mid));
        assert!(!reaches.contains(&dead));
        assert!(!reaches.contains(&loop_a));
        assert!(!reaches.contains(&loop_b));
    }

    #[test]
    fn catalog_lists_monsters_whose_refs_reach_targets() {
        // Neither monster references a patch target directly; both reference
        // .epk packs. Only the pack that transitively attaches a target
        // makes its monster a catalog row.
        let reaching_pack = "metadata/effects/spells/monsters_effects/act1/boghulk/spawn.epk";
        let monster_files: Vec<(String, Vec<u8>)> = vec![
            (
                "metadata/monsters/boghulk/boghulk".to_string(),
                utf16_file(
                    r#"epk = "Metadata/Effects/Spells/monsters_effects/act1/boghulk/spawn.epk""#,
                ),
            ),
            (
                "metadata/monsters/bird/mutantbird".to_string(),
                utf16_file(r#"epk = "Metadata/Effects/Utility/epks/dead.epk""#),
            ),
        ];
        let edges = collect_edges(&monster_files, None, &[]);
        let reaching: BTreeSet<String> = [reaching_pack.to_string()].into();

        let rows = catalog_rows(&edges, None, &reaching);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].monster_keys, vec!["metadata/monsters/boghulk/boghulk".to_string()]);
    }

    #[test]
    fn catalog_splits_shared_display_names_by_family() {
        // Unrelated monsters named "Daemon" live in different family
        // folders and must not share one checkbox; true variants inside one
        // family still group.
        let monster_files: Vec<(String, Vec<u8>)> = vec![
            (
                "metadata/monsters/daemon/bossdaemon".to_string(),
                utf16_file(r#"rig = "Metadata/Effects/Spells/monsters_effects/a/one.ao""#),
            ),
            (
                "metadata/monsters/leaguedelirium/daemonspawner".to_string(),
                utf16_file(r#"rig = "Metadata/Effects/Spells/monsters_effects/b/two.ao""#),
            ),
        ];
        let varieties = table(&[
            &["Metadata/Monsters/Daemon/BossDaemon", "Mob Daemon"],
            &["Metadata/Monsters/LeagueDelirium/DaemonSpawner", "Mob Daemon"],
        ]);

        let rows = monster_effect_catalog_from_parts(&monster_files, Some(&varieties), &[]);

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.display == "Mob Daemon"));
        assert_eq!(rows[0].family, "daemon");
        assert_eq!(rows[1].family, "leaguedelirium");
        assert_eq!(rows[0].monster_keys.len(), 1);
        assert_eq!(rows[1].monster_keys.len(), 1);
    }

    #[test]
    fn normalized_metadata_path_trims_embedded_noise() {
        assert_eq!(
            normalized_metadata_path(
                r#"AddEffectPack( "Metadata/Effects/Spells/x/y.ao" );"#
            )
            .as_deref(),
            Some("metadata/effects/spells/x/y.ao")
        );
        assert_eq!(
            normalized_metadata_path("Metadata\\Effects\\Spells\\X\\Y.AOC").as_deref(),
            Some("metadata/effects/spells/x/y.aoc")
        );
        assert_eq!(normalized_metadata_path("no path here"), None);
    }
}
