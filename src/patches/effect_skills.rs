use super::datc64::{parse_table, utf16le_string_at};
use super::targeting::{is_metadata_effect_ext, starts_with_path_ci};
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

const SPELLS_PREFIX: &str = "metadata/effects/spells/";
const MIN_LABEL_ALIAS_LEN: usize = 4;

/// How the `Effects` patch treats one top-level skill folder under
/// `metadata/effects/spells/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectLevel {
    /// Leave every file of the skill untouched (original visuals).
    Full,
    /// Strip nonessential client blocks from `.ao`/`.aoc` (the patch's
    /// long-standing behavior).
    #[default]
    Reduced,
}

impl<'de> Deserialize<'de> for EffectLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(EffectLevelVisitor)
    }
}

struct EffectLevelVisitor;

impl<'de> Visitor<'de> for EffectLevelVisitor {
    type Value = EffectLevel;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an effect level")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "full" => Ok(EffectLevel::Full),
            "reduced" | OLD_EFFECT_LEVEL => Ok(EffectLevel::Reduced),
            _ => Err(E::unknown_variant(value, &["full", "reduced"])),
        }
    }
}

const OLD_EFFECT_LEVEL: &str = concat!("hid", "den");

/// One persisted non-default per-skill setting. `folder` is the lowercase
/// top-level segment after `metadata/effects/spells/`, e.g.
/// "cold_herald_of_ice". Folders without an override are `Reduced`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectSkillOverride {
    pub folder: String,
    pub level: EffectLevel,
}

/// One live ActiveSkills-backed row in the per-skill effects editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSkillCatalogEntry {
    pub active_skill_id: String,
    pub display: String,
    pub action_type: String,
    pub folders: Vec<String>,
}

/// Per-request view over the overrides. `None` (no overrides at all) keeps
/// target collection and transforms byte-identical to the unfiltered
/// behavior. `full_paths` holds the per-monster exclusions: exact normalized
/// lowercase patch-target paths kept original (see `monster_effects`).
#[derive(Debug)]
pub(super) struct EffectsFilter {
    full_folders: BTreeSet<String>,
    full_paths: BTreeSet<String>,
}

impl EffectsFilter {
    pub(super) fn new(
        overrides: &[EffectSkillOverride],
        full_paths: BTreeSet<String>,
    ) -> Option<Self> {
        let full_folders: BTreeSet<_> = overrides
            .iter()
            .filter(|entry| entry.level == EffectLevel::Full)
            .map(|entry| entry.folder.to_ascii_lowercase())
            .collect();
        (!full_folders.is_empty() || !full_paths.is_empty()).then_some(Self {
            full_folders,
            full_paths,
        })
    }

    pub(super) fn level_for(&self, path: &str) -> EffectLevel {
        if let Some(folder) = spells_folder(path) {
            if self.full_folders.contains(&folder.to_ascii_lowercase()) {
                return EffectLevel::Full;
            }
        }
        // Only allocates for paths already under `metadata/effects/spells/`
        // (all callers pre-filter) and only when monster overrides exist.
        if !self.full_paths.is_empty()
            && self
                .full_paths
                .contains(&super::targeting::normalize_path(path))
        {
            return EffectLevel::Full;
        }
        EffectLevel::Reduced
    }

    pub(super) fn has_full(&self) -> bool {
        !self.full_folders.is_empty() || !self.full_paths.is_empty()
    }
}

/// The top-level folder segment after `metadata/effects/spells/`
/// (case/backslash-insensitive), or `None` for paths outside the prefix and
/// for files sitting directly in `spells/` — those always behave as
/// `Reduced` and are not overridable.
pub(super) fn spells_folder(path: &str) -> Option<&str> {
    if !starts_with_path_ci(path, SPELLS_PREFIX) {
        return None;
    }
    let rest = &path[SPELLS_PREFIX.len()..];
    let end = rest.bytes().position(|b| b == b'/' || b == b'\\')?;
    (end > 0).then(|| &rest[..end])
}

/// Distinct sorted lowercase top-level spell folders that contain at least
/// one effect data file — the catalog behind the per-skill editor.
pub fn effect_skill_folders(paths: &[String]) -> Vec<String> {
    let mut folders: Vec<String> = paths
        .iter()
        .filter(|path| is_metadata_effect_ext(path))
        .filter_map(|path| spells_folder(path))
        .map(str::to_ascii_lowercase)
        .collect();
    folders.sort_unstable();
    folders.dedup();
    folders
}

/// Build the per-skill effects catalog from the game's own skill tables.
/// Folder paths are used only to validate that a resolved folder exists and
/// contains effect files the patch can target.
pub fn build_effect_skill_catalog(
    activeskills_bytes: &[u8],
    actiontypes_bytes: &[u8],
    item_visual_effect_bytes: Option<&[u8]>,
    miscanimated_bytes: Option<&[u8]>,
    paths: &[String],
) -> Option<Vec<EffectSkillCatalogEntry>> {
    let skills = active_skills_with_actions(activeskills_bytes, actiontypes_bytes)?;
    let valid_folders: BTreeSet<String> = effect_skill_folders(paths).into_iter().collect();
    if valid_folders.is_empty() {
        return Some(Vec::new());
    }

    let mut skills_by_action: HashMap<String, Vec<&ActiveSkillEffectRow>> = HashMap::new();
    for skill in &skills {
        skills_by_action
            .entry(skill.action_key.clone())
            .or_default()
            .push(skill);
    }

    let mut candidates: BTreeMap<String, BTreeMap<String, FolderCandidate>> = BTreeMap::new();
    if let Some(bytes) = item_visual_effect_bytes {
        collect_itemvisual_catalog_candidates(
            bytes,
            &skills_by_action,
            &valid_folders,
            &mut candidates,
        );
    }
    if let Some(bytes) = miscanimated_bytes {
        collect_miscanimated_catalog_candidates(bytes, &skills, &valid_folders, &mut candidates);
    }
    collect_folder_name_catalog_candidates(&skills, &valid_folders, &mut candidates);

    let mut by_skill: BTreeMap<String, EffectSkillCatalogEntry> = BTreeMap::new();
    for (folder, skill_candidates) in candidates {
        let Some(candidate) = select_folder_candidate(&folder, &skill_candidates) else {
            continue;
        };

        let row = by_skill
            .entry(candidate.active_skill_id.clone())
            .or_insert_with(|| EffectSkillCatalogEntry {
                active_skill_id: candidate.active_skill_id.clone(),
                display: candidate.display.clone(),
                action_type: candidate.action_type.clone(),
                folders: Vec::new(),
            });
        push_unique(&mut row.folders, folder);
    }

    let mut rows: Vec<_> = by_skill
        .into_values()
        .filter_map(|mut row| {
            row.folders.sort();
            (!row.folders.is_empty()).then_some(row)
        })
        .collect();
    rows.sort_by(|a, b| {
        a.display
            .to_lowercase()
            .cmp(&b.display.to_lowercase())
            .then_with(|| a.active_skill_id.cmp(&b.active_skill_id))
    });
    Some(rows)
}

#[derive(Debug)]
struct ActiveSkillEffectRow {
    active_skill_id: String,
    display: String,
    action_type: String,
    action_key: String,
}

impl ActiveSkillEffectRow {
    fn label_aliases(&self) -> Vec<(String, AliasRank)> {
        let mut aliases = Vec::new();
        push_unique_alias(&mut aliases, self.action_key.clone(), AliasRank::Action);
        push_min_len_alias(
            &mut aliases,
            normalize_label_key(&self.display),
            AliasRank::Display,
        );
        push_min_len_alias(
            &mut aliases,
            normalize_label_key(&self.active_skill_id),
            AliasRank::Id,
        );
        if self.action_type.ends_with("New") {
            if let Some(stripped) = self.action_key.strip_suffix("new") {
                push_min_len_alias(&mut aliases, stripped.to_string(), AliasRank::Action);
            }
        }
        aliases
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AliasRank {
    Id,
    Display,
    Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EvidenceConfidence {
    FolderName,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FolderNameMatch {
    None,
    Affix,
    StrongAffix,
    Exact,
}

#[derive(Debug)]
struct FolderCandidate {
    active_skill_id: String,
    display: String,
    action_type: String,
    confidence: EvidenceConfidence,
    alias_rank: AliasRank,
    match_len: usize,
    folder_match: FolderNameMatch,
}

impl FolderCandidate {
    fn score(&self) -> (u8, FolderNameMatch, usize) {
        candidate_score(
            self.confidence,
            self.alias_rank,
            self.folder_match,
            self.match_len,
        )
    }
}

fn candidate_score(
    confidence: EvidenceConfidence,
    alias_rank: AliasRank,
    folder_match: FolderNameMatch,
    match_len: usize,
) -> (u8, FolderNameMatch, usize) {
    let source_rank = match (confidence, alias_rank, folder_match) {
        (EvidenceConfidence::High, _, _) => 6,
        (EvidenceConfidence::FolderName, _, FolderNameMatch::Exact) => 5,
        (EvidenceConfidence::FolderName, _, FolderNameMatch::StrongAffix) => 4,
        (EvidenceConfidence::Medium, AliasRank::Action, _) => 3,
        (EvidenceConfidence::FolderName, _, FolderNameMatch::Affix) => 2,
        (EvidenceConfidence::Medium, AliasRank::Display, _) => 1,
        (EvidenceConfidence::Medium, AliasRank::Id, _) => 0,
        (EvidenceConfidence::FolderName, _, FolderNameMatch::None) => 0,
    };
    (source_rank, folder_match, match_len)
}

fn active_skills_with_actions(
    activeskills_bytes: &[u8],
    actiontypes_bytes: &[u8],
) -> Option<Vec<ActiveSkillEffectRow>> {
    let (display_names, action_refs) = parse_activeskill_rows(activeskills_bytes)?;
    let action_types = parse_first_string_rows(actiontypes_bytes)?;
    let mut skills = Vec::new();
    for (active_skill_id, action_idx) in action_refs {
        let Some(display) = display_names.get(&active_skill_id) else {
            continue;
        };
        let Some(action_type) = action_types.get(action_idx).filter(|s| !s.is_empty()) else {
            continue;
        };
        let action_key = normalize_label_key(action_type);
        if action_key.is_empty() {
            continue;
        }
        skills.push(ActiveSkillEffectRow {
            active_skill_id,
            display: display.clone(),
            action_type: action_type.clone(),
            action_key,
        });
    }
    (!skills.is_empty()).then_some(skills)
}

fn collect_itemvisual_catalog_candidates(
    bytes: &[u8],
    skills_by_action: &HashMap<String, Vec<&ActiveSkillEffectRow>>,
    valid_folders: &BTreeSet<String>,
    candidates: &mut BTreeMap<String, BTreeMap<String, FolderCandidate>>,
) {
    let Some(table) = parse_table(bytes) else {
        return;
    };
    let heap = table.heap();
    for row in table.rows() {
        let Some(label) = row
            .get(0..8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .and_then(|off| utf16le_string_at(heap, off as usize))
        else {
            continue;
        };
        let Some(action_key) = effect_label_action_name(&label).map(normalize_label_key) else {
            continue;
        };
        let Some(skills) = skills_by_action.get(&action_key) else {
            continue;
        };
        for folder in effect_folders_in_row(row, heap, valid_folders) {
            for skill in skills {
                insert_folder_candidate(
                    candidates,
                    &folder,
                    skill,
                    EvidenceConfidence::High,
                    AliasRank::Action,
                    action_key.len(),
                    FolderNameMatch::None,
                );
            }
        }
    }
}

fn collect_miscanimated_catalog_candidates(
    bytes: &[u8],
    skills: &[ActiveSkillEffectRow],
    valid_folders: &BTreeSet<String>,
    candidates: &mut BTreeMap<String, BTreeMap<String, FolderCandidate>>,
) {
    let Some(table) = parse_table(bytes) else {
        return;
    };
    let skill_aliases: Vec<_> = skills
        .iter()
        .map(|skill| (skill, skill.label_aliases()))
        .collect();
    let heap = table.heap();
    for row in table.rows() {
        let Some(label) = row
            .get(0..8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .and_then(|off| utf16le_string_at(heap, off as usize))
        else {
            continue;
        };
        let label_key = normalize_label_key(&label);
        let folders = effect_folders_in_row(row, heap, valid_folders);
        if folders.is_empty() {
            continue;
        }
        for (skill, aliases) in &skill_aliases {
            if skill.action_key == "donothing" {
                continue;
            }
            let Some(match_len) = aliases
                .iter()
                .filter(|(alias, _)| label_key.starts_with(alias))
                .map(|(alias, rank)| (alias.len(), *rank))
                .max()
            else {
                continue;
            };
            for folder in &folders {
                insert_folder_candidate(
                    candidates,
                    folder,
                    skill,
                    EvidenceConfidence::Medium,
                    match_len.1,
                    match_len.0,
                    FolderNameMatch::None,
                );
            }
        }
    }
}

fn collect_folder_name_catalog_candidates(
    skills: &[ActiveSkillEffectRow],
    valid_folders: &BTreeSet<String>,
    candidates: &mut BTreeMap<String, BTreeMap<String, FolderCandidate>>,
) {
    let skill_display_keys: Vec<_> = skills
        .iter()
        .filter_map(|skill| {
            let display_key = normalize_label_key(&skill.display);
            (!display_key.is_empty()).then_some((skill, display_key))
        })
        .collect();
    for folder in valid_folders {
        let folder_key = normalize_label_key(folder);
        if folder_key.is_empty() {
            continue;
        }
        for (skill, display_key) in &skill_display_keys {
            if skill.action_key == "donothing" {
                continue;
            }
            let id_key = normalize_label_key(&skill.active_skill_id);
            let Some((folder_match, match_len)) =
                folder_name_match(display_key, &id_key, &folder_key)
            else {
                continue;
            };
            if folder_match == FolderNameMatch::Affix && match_len < 6 {
                continue;
            }
            {
                insert_folder_candidate(
                    candidates,
                    folder,
                    skill,
                    EvidenceConfidence::FolderName,
                    AliasRank::Display,
                    match_len,
                    folder_match,
                );
            }
        }
    }
}

fn folder_name_match(
    display_key: &str,
    id_key: &str,
    folder_key: &str,
) -> Option<(FolderNameMatch, usize)> {
    if display_key == folder_key {
        Some((FolderNameMatch::Exact, display_key.len()))
    } else if display_key.ends_with(folder_key) || folder_key.ends_with(display_key) {
        let match_kind = if id_key == folder_key
            || id_key.ends_with(folder_key)
            || folder_key.ends_with(id_key)
            || id_key.starts_with(folder_key)
            || folder_key.starts_with(id_key)
        {
            FolderNameMatch::StrongAffix
        } else {
            FolderNameMatch::Affix
        };
        Some((match_kind, display_key.len().min(folder_key.len())))
    } else {
        None
    }
}

fn effect_folders_in_row(row: &[u8], heap: &[u8], valid_folders: &BTreeSet<String>) -> Vec<String> {
    let mut folders = Vec::new();
    for field in (0..row.len()).step_by(4) {
        if field + 8 <= row.len() {
            let off = u64::from_le_bytes(row[field..field + 8].try_into().unwrap()) as usize;
            if let Some(folder) = utf16le_string_at(heap, off)
                .and_then(|path| effect_folder_from_metadata_path(&path))
                .filter(|folder| valid_folders.contains(folder))
            {
                push_unique(&mut folders, folder);
            }
        }
        if field + 4 <= row.len() {
            let off = u32::from_le_bytes(row[field..field + 4].try_into().unwrap()) as usize;
            if let Some(folder) = utf16le_string_at(heap, off)
                .and_then(|path| effect_folder_from_metadata_path(&path))
                .filter(|folder| valid_folders.contains(folder))
            {
                push_unique(&mut folders, folder);
            }
        }
    }
    folders
}

fn insert_folder_candidate(
    candidates: &mut BTreeMap<String, BTreeMap<String, FolderCandidate>>,
    folder: &str,
    skill: &ActiveSkillEffectRow,
    confidence: EvidenceConfidence,
    alias_rank: AliasRank,
    match_len: usize,
    folder_match: FolderNameMatch,
) {
    candidates
        .entry(folder.to_string())
        .or_default()
        .entry(skill.active_skill_id.clone())
        .and_modify(|candidate| {
            let incoming_score = candidate_score(confidence, alias_rank, folder_match, match_len);
            if incoming_score > candidate.score() {
                candidate.confidence = confidence;
                candidate.alias_rank = alias_rank;
                candidate.match_len = match_len;
                candidate.folder_match = folder_match;
            }
        })
        .or_insert_with(|| FolderCandidate {
            active_skill_id: skill.active_skill_id.clone(),
            display: skill.display.clone(),
            action_type: skill.action_type.clone(),
            confidence,
            alias_rank,
            match_len,
            folder_match,
        });
}

fn select_folder_candidate<'a>(
    folder: &str,
    skill_candidates: &'a BTreeMap<String, FolderCandidate>,
) -> Option<&'a FolderCandidate> {
    let best_score = skill_candidates
        .values()
        .map(FolderCandidate::score)
        .max()?;
    let best_candidates: Vec<_> = skill_candidates
        .values()
        .filter(|candidate| candidate.score() == best_score)
        .collect();
    match best_candidates.as_slice() {
        [] => None,
        [candidate] => Some(*candidate),
        _ => select_same_display_candidate(folder, &best_candidates),
    }
}

fn select_same_display_candidate<'a>(
    folder: &str,
    candidates: &[&'a FolderCandidate],
) -> Option<&'a FolderCandidate> {
    let display = &candidates.first()?.display;
    if candidates
        .iter()
        .any(|candidate| candidate.display != *display)
    {
        return None;
    }

    let folder_key = normalize_label_key(folder);
    candidates.iter().copied().min_by(|a, b| {
        same_display_tiebreak_key(a, &folder_key).cmp(&same_display_tiebreak_key(b, &folder_key))
    })
}

fn same_display_tiebreak_key<'a>(
    candidate: &'a FolderCandidate,
    folder_key: &str,
) -> (u8, u8, usize, &'a str) {
    let id_key = normalize_label_key(&candidate.active_skill_id);
    let folder_relation = if id_key == folder_key {
        0
    } else if id_key.ends_with(folder_key)
        || folder_key.ends_with(&id_key)
        || id_key.starts_with(folder_key)
        || folder_key.starts_with(&id_key)
    {
        1
    } else {
        2
    };
    let variant_penalty = if candidate.active_skill_id.starts_with("new_")
        || candidate.active_skill_id.ends_with("_new")
        || candidate.active_skill_id.contains("_new_")
    {
        1
    } else {
        0
    };
    (
        folder_relation,
        variant_penalty,
        candidate.active_skill_id.len(),
        candidate.active_skill_id.as_str(),
    )
}

pub const ACTIVESKILLS_DATC64_PATH: &str = "data/balance/activeskills.datc64";
pub const ACTIONTYPES_DATC64_PATH: &str = "data/balance/actiontypes.datc64";
pub const ITEM_VISUAL_EFFECT_DATC64_PATH: &str = "data/balance/itemvisualeffect.datc64";
type ActiveSkillRows = (HashMap<String, String>, Vec<(String, usize)>);

fn parse_first_string_rows(bytes: &[u8]) -> Option<Vec<String>> {
    let table = parse_table(bytes)?;
    let heap = table.heap();
    let mut rows = Vec::with_capacity(table.row_count);
    for row in table.rows() {
        let value = row
            .get(0..8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .and_then(|off| utf16le_string_at(heap, off as usize))
            .unwrap_or_default();
        rows.push(value);
    }
    Some(rows)
}

fn parse_activeskill_rows(bytes: &[u8]) -> Option<ActiveSkillRows> {
    let table = parse_table(bytes)?;
    let row_len = table.row_len;
    if row_len < 16 {
        return None;
    }
    let heap = table.heap();

    let mut map = HashMap::with_capacity(table.row_count);
    let mut action_refs = Vec::with_capacity(table.row_count);
    for row in table.rows() {
        // `row.len() == row_len >= 16` (chunks_exact + the check above), so
        // these 8-byte slices always convert.
        let id_off = u64::from_le_bytes(row[0..8].try_into().unwrap()) as usize;
        let name_off = u64::from_le_bytes(row[8..16].try_into().unwrap()) as usize;
        let Some(id) = utf16le_string_at(heap, id_off).filter(|s| is_plausible_skill_id(s)) else {
            continue;
        };
        let Some(name) = utf16le_string_at(heap, name_off).and_then(|s| clean_display_name(&s))
        else {
            continue;
        };
        let id = id.to_ascii_lowercase();
        if row_len >= 28 {
            if let Some(action_idx) = row
                .get(24..28)
                .and_then(|b| b.try_into().ok())
                .map(u32::from_le_bytes)
                .map(|n| n as usize)
            {
                action_refs.push((id.clone(), action_idx));
            }
        }
        map.insert(id, name);
    }
    (!map.is_empty()).then_some((map, action_refs))
}

fn effect_label_action_name(label: &str) -> Option<&str> {
    label
        .strip_prefix("Skill_")
        .or_else(|| label.strip_prefix("Surge_"))
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 80
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

fn effect_folder_from_metadata_path(path: &str) -> Option<String> {
    spells_folder(path).map(str::to_ascii_lowercase)
}

fn normalize_label_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn push_unique_alias(values: &mut Vec<(String, AliasRank)>, value: String, rank: AliasRank) {
    if value.is_empty() {
        return;
    }
    match values.iter_mut().find(|(existing, _)| existing == &value) {
        Some((_, existing_rank)) => {
            *existing_rank = (*existing_rank).max(rank);
        }
        None => values.push((value, rank)),
    }
}

fn push_min_len_alias(values: &mut Vec<(String, AliasRank)>, value: String, rank: AliasRank) {
    if value.len() >= MIN_LABEL_ALIAS_LEN {
        push_unique_alias(values, value, rank);
    }
}

fn is_plausible_skill_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_plausible_display_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.chars().all(|c| !c.is_control())
}

/// Leading bracketed tags (`[DNT]`, `[DNT-UNUSED]`, `[UNUSED]`) are not a
/// reliability signal: live skills like Smite carry them. Strip the tag and
/// trust the remainder unless it reads as a dead/placeholder marker — of
/// 113 tagged rows on a live install, only 4 matched such markers
/// ("DISCONTINUED", "(NOT CURRENTLY USED)", "A placeholder self-buff").
pub(super) fn clean_display_name(name: &str) -> Option<String> {
    let stripped = match name.strip_prefix('[') {
        Some(rest) => rest.split_once(']')?.1.trim_start(),
        None => name,
    };
    if !is_plausible_display_name(stripped) {
        return None;
    }
    const DEAD_MARKERS: [&str; 6] = [
        "discontinued",
        "not currently used",
        "placeholder",
        "deprecated",
        "removed",
        "unused",
    ];
    let low = stripped.to_ascii_lowercase();
    if DEAD_MARKERS.iter().any(|marker| low.contains(marker)) {
        return None;
    }
    Some(stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ROW_MARKER: [u8; 8] = [0xbb; 8];

    #[test]
    fn spells_folder_extracts_top_level_segment_across_spellings() {
        for (path, expected) in [
            (
                "metadata/effects/spells/cold_herald_of_ice/ao/ice_explosion.ao",
                Some("cold_herald_of_ice"),
            ),
            (
                "Metadata\\Effects\\Spells\\Cold_Herald_Of_Ice\\epk\\buff.epk",
                Some("Cold_Herald_Of_Ice"),
            ),
            ("metadata/effects/spells/arc_02/arc.aoc", Some("arc_02")),
            // Files directly in spells/ have no folder to override.
            ("metadata/effects/spells/loose_file.ao", None),
            ("metadata/particles/enviro/foo.pet", None),
            ("data/statdescriptions/stat_descriptions.csd", None),
        ] {
            assert_eq!(spells_folder(path), expected, "path: {path}");
        }
    }

    #[test]
    fn filter_resolves_levels_case_insensitively_and_defaults_to_reduced() {
        assert!(EffectsFilter::new(&[], BTreeSet::new()).is_none());
        // Explicit Reduced entries are meaningless and collapse to None.
        assert!(EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "arc_02".to_string(),
                level: EffectLevel::Reduced,
            }],
            BTreeSet::new()
        )
        .is_none());

        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "cold_herald_of_ice".to_string(),
                level: EffectLevel::Full,
            }],
            BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            filter.level_for("Metadata/Effects/Spells/Cold_Herald_Of_Ice/ao/ice_explosion.ao"),
            EffectLevel::Full
        );
        assert_eq!(
            filter.level_for("metadata/effects/spells/arc_02/beam.pet"),
            EffectLevel::Reduced
        );
        assert_eq!(
            filter.level_for("metadata/effects/spells/fireball/fireball.ao"),
            EffectLevel::Reduced
        );
        assert_eq!(
            filter.level_for("metadata/effects/spells/loose_file.ao"),
            EffectLevel::Reduced
        );
    }

    #[test]
    fn filter_resolves_monster_paths_exactly_and_composes_with_folders() {
        let monster_paths: BTreeSet<String> = [
            "metadata/effects/spells/monsters_effects/act3/anchoritemother/idle.ao".to_string(),
        ]
        .into();
        // Monster paths alone are enough to construct a filter.
        let filter = EffectsFilter::new(&[], monster_paths.clone()).unwrap();
        assert!(filter.has_full());
        assert_eq!(
            filter.level_for(
                "Metadata\\Effects\\Spells\\monsters_effects\\act3\\AnchoriteMother\\idle.ao"
            ),
            EffectLevel::Full
        );
        // Sibling files of the same monster folder stay Reduced.
        assert_eq!(
            filter.level_for("metadata/effects/spells/monsters_effects/act3/anchoritemother/death.ao"),
            EffectLevel::Reduced
        );

        // Skill folders and monster paths compose.
        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }],
            monster_paths,
        )
        .unwrap();
        assert_eq!(
            filter.level_for("metadata/effects/spells/fireball/fireball.ao"),
            EffectLevel::Full
        );
        assert_eq!(
            filter.level_for(
                "metadata/effects/spells/monsters_effects/act3/anchoritemother/idle.ao"
            ),
            EffectLevel::Full
        );
        assert_eq!(
            filter.level_for("metadata/effects/spells/arc_02/arc.aoc"),
            EffectLevel::Reduced
        );
    }

    #[test]
    fn effect_skill_folders_dedup_sort_and_skip_non_effect_files() {
        let paths: Vec<String> = [
            "metadata/effects/spells/fireball/fireball.ao",
            "metadata/effects/spells/fireball/fx/impact.pet",
            "Metadata/Effects/Spells/Arc_02/beam.trl",
            "metadata/effects/spells/readme/notes.txt",
            "metadata/effects/spells/loose_file.ao",
            "metadata/particles/enviro/foo.pet",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(effect_skill_folders(&paths), vec!["arc_02", "fireball"]);
    }

    fn build_activeskills_with_actions(rows: &[(&str, &str, u32)]) -> Vec<u8> {
        let mut heap = TEST_ROW_MARKER.to_vec();
        let mut offsets = Vec::new();
        for (id, name, action_idx) in rows {
            let id_off = push_utf16(&mut heap, id);
            let name_off = push_utf16(&mut heap, name);
            offsets.push((id_off, name_off, *action_idx));
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for (id_off, name_off, action_idx) in offsets {
            let mut row = vec![0u8; 28];
            row[0..8].copy_from_slice(&id_off.to_le_bytes());
            row[8..16].copy_from_slice(&name_off.to_le_bytes());
            row[24..28].copy_from_slice(&action_idx.to_le_bytes());
            bytes.extend_from_slice(&row);
        }
        bytes.extend_from_slice(&heap);
        bytes
    }

    fn build_actiontypes_bytes(rows: &[&str]) -> Vec<u8> {
        let mut heap = TEST_ROW_MARKER.to_vec();
        let offsets: Vec<_> = rows.iter().map(|id| push_utf16(&mut heap, id)).collect();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for id_off in offsets {
            bytes.extend_from_slice(&id_off.to_le_bytes());
        }
        bytes.extend_from_slice(&heap);
        bytes
    }

    fn build_item_visual_effect_bytes(rows: &[(&str, &str)]) -> Vec<u8> {
        build_label_path_table_bytes(rows)
    }

    fn build_miscanimated_bytes(rows: &[(&str, &str)]) -> Vec<u8> {
        build_label_path_table_bytes(rows)
    }

    fn build_label_path_table_bytes(rows: &[(&str, &str)]) -> Vec<u8> {
        let mut heap = TEST_ROW_MARKER.to_vec();
        let offsets: Vec<_> = rows
            .iter()
            .map(|(label, path)| (push_utf16(&mut heap, label), push_utf16(&mut heap, path)))
            .collect();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for (label_off, path_off) in offsets {
            bytes.extend_from_slice(&label_off.to_le_bytes());
            bytes.extend_from_slice(&path_off.to_le_bytes());
        }
        bytes.extend_from_slice(&heap);
        bytes
    }

    fn effect_paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    fn row<'a>(
        rows: &'a [EffectSkillCatalogEntry],
        active_skill_id: &str,
    ) -> &'a EffectSkillCatalogEntry {
        rows.iter()
            .find(|row| row.active_skill_id == active_skill_id)
            .unwrap()
    }

    fn push_utf16(heap: &mut Vec<u8>, s: &str) -> u64 {
        let offset = heap.len() as u64;
        for unit in s.encode_utf16() {
            heap.extend_from_slice(&unit.to_le_bytes());
        }
        heap.extend_from_slice(&0u16.to_le_bytes());
        offset
    }

    #[test]
    fn effect_catalog_returns_empty_when_no_effect_folders_exist() {
        let activeskills = build_activeskills_with_actions(&[("ice_shot", "Ice Shot", 0)]);
        let actiontypes = build_actiontypes_bytes(&["IceShot"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "IceShotCone",
            "Metadata/Effects/Spells/bow_ice_shot/cone_impact.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/bow_ice_shot/readme.txt"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_resolves_split_skill_folders_from_live_tables() {
        let activeskills =
            build_activeskills_with_actions(&[("herald_of_ash", "Herald of Ash", 0)]);
        let actiontypes = build_actiontypes_bytes(&["HeraldOfAsh"]);
        let itemvisual = build_item_visual_effect_bytes(&[(
            "Skill_HeraldOfAsh",
            "Metadata/Effects/Spells/fire_heraldofash/epk/weapon_buff.epk",
        )]);
        let miscanimated = build_miscanimated_bytes(&[(
            "HeraldOfAshExplosion",
            "Metadata/Effects/Spells/herald_of_fire/onkill_AoE_01.ao",
        )]);
        let paths = effect_paths(&[
            "metadata/effects/spells/fire_heraldofash/epk/weapon_buff.epk",
            "metadata/effects/spells/herald_of_fire/onkill_AoE_01.ao",
        ]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            Some(&itemvisual),
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active_skill_id, "herald_of_ash");
        assert_eq!(rows[0].display, "Herald of Ash");
        assert_eq!(rows[0].action_type, "HeraldOfAsh");
        assert_eq!(
            rows[0].folders,
            vec!["fire_heraldofash".to_string(), "herald_of_fire".to_string()]
        );
    }

    #[test]
    fn effect_catalog_resolves_ice_shot_effect_parts() {
        let activeskills = build_activeskills_with_actions(&[("ice_shot", "Ice Shot", 0)]);
        let actiontypes = build_actiontypes_bytes(&["IceShot"]);
        let miscanimated = build_miscanimated_bytes(&[
            (
                "IceShotCone",
                "Metadata/Effects/Spells/bow_ice_shot/cone_impact.ao",
            ),
            (
                "IceShotShardImpact",
                "Metadata/Effects/Spells/bow_ice_shot/fusillade_impact.ao",
            ),
        ]);
        let paths = effect_paths(&[
            "metadata/effects/spells/bow_ice_shot/cone_impact.ao",
            "metadata/effects/spells/bow_ice_shot/fusillade_impact.ao",
        ]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active_skill_id, "ice_shot");
        assert_eq!(rows[0].display, "Ice Shot");
        assert_eq!(rows[0].action_type, "IceShot");
        assert_eq!(rows[0].folders, vec!["bow_ice_shot".to_string()]);
    }

    #[test]
    fn malformed_action_type_does_not_make_folders_ambiguous() {
        // "ice_shot" resolves normally; "broken" has action type "_" which
        // normalizes to "" and must not match every label.
        let activeskills = build_activeskills_with_actions(&[
            ("ice_shot", "Ice Shot", 0),
            ("broken", "Broken Skill", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["IceShot", "_"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "IceShotCone",
            "Metadata/Effects/Spells/bow_ice_shot/cone_impact.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/bow_ice_shot/cone_impact.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        // Without the fix, "broken" (empty action_key) also matches "IceShotCone",
        // making bow_ice_shot ambiguous and the result empty.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active_skill_id, "ice_shot");
    }

    #[test]
    fn effect_catalog_accepts_any_action_prefixed_animation_label() {
        let activeskills =
            build_activeskills_with_actions(&[("freezing_salvo", "Freezing Salvo", 0)]);
        let actiontypes = build_actiontypes_bytes(&["FreezingSalvo"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "FreezingSalvoCompletelyNewEffectPart",
            "Metadata/Effects/Spells/bow_freezingsalvo/new_part.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/bow_freezingsalvo/new_part.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active_skill_id, "freezing_salvo");
        assert_eq!(rows[0].folders, vec!["bow_freezingsalvo".to_string()]);
    }

    #[test]
    fn effect_catalog_uses_active_skill_alias_before_shorter_stale_action() {
        let activeskills = build_activeskills_with_actions(&[
            ("elemental_storm", "Elemental Storm", 0),
            ("tornado_shot", "Tornado Shot", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Tornado", "TornadoShotNew"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "TornadoShotImpact",
            "Metadata/Effects/Spells/bow_tornado_shot/impact.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/bow_tornado_shot/impact.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(
            row(&rows, "tornado_shot").folders,
            vec!["bow_tornado_shot".to_string()]
        );
        assert!(!rows
            .iter()
            .any(|row| row.active_skill_id == "elemental_storm"));
    }

    #[test]
    fn effect_catalog_does_not_strip_bare_new_from_action_words() {
        let activeskills = build_activeskills_with_actions(&[("renew", "Renew", 0)]);
        let actiontypes = build_actiontypes_bytes(&["Renew"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "ReImpact",
            "Metadata/Effects/Spells/re_impact/impact.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/re_impact/impact.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        let aliases = ActiveSkillEffectRow {
            active_skill_id: "renew".to_string(),
            display: "Renew".to_string(),
            action_type: "Renew".to_string(),
            action_key: "renew".to_string(),
        }
        .label_aliases();
        assert!(!aliases.iter().any(|(alias, _)| alias == "re"));
        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_ignores_empty_display_key_for_folder_name_fallback() {
        let activeskills = build_activeskills_with_actions(&[("empty_display", "???", 0)]);
        let actiontypes = build_actiontypes_bytes(&["Placeholder"]);
        let paths = effect_paths(&["metadata/effects/spells/empty_display/rig.ao"]);

        let rows =
            build_effect_skill_catalog(&activeskills, &actiontypes, None, None, &paths).unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_ignores_short_display_aliases_for_animation_labels() {
        let activeskills = build_activeskills_with_actions(&[("short_display", "Re", 0)]);
        let actiontypes = build_actiontypes_bytes(&["Placeholder"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "ReImpact",
            "Metadata/Effects/Spells/re_impact/impact.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/re_impact/impact.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_keeps_active_skill_visible_and_uses_folder_name_fallback() {
        let activeskills = build_activeskills_with_actions(&[
            ("tempest_flurry", "Tempest Flurry", 0),
            ("charged_attack_channel", "Blade Flurry", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["TempestFlurry", "DoNothing"]);
        let miscanimated =
            build_miscanimated_bytes(&[("BladeFlurry", "Metadata/Effects/Spells/flurry/rig.ao")]);
        let paths = effect_paths(&["metadata/effects/spells/flurry/rig.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(
            row(&rows, "tempest_flurry").folders,
            vec!["flurry".to_string()]
        );
        assert!(!rows
            .iter()
            .any(|row| row.active_skill_id == "charged_attack_channel"));
    }

    #[test]
    fn effect_catalog_prefers_action_alias_over_duplicate_display_alias() {
        let activeskills = build_activeskills_with_actions(&[
            ("avian_tornado", "Twister", 0),
            ("twister", "Twister", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Spark", "Twister"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "TwisterChilled",
            "Metadata/Effects/Spells/spear_tornadoes/twister.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/spear_tornadoes/twister.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(
            row(&rows, "twister").folders,
            vec!["spear_tornadoes".to_string()]
        );
        assert!(!rows
            .iter()
            .any(|row| row.active_skill_id == "avian_tornado"));
        let twister_rows: Vec<_> = rows
            .iter()
            .filter(|row| row.display == "Twister")
            .map(|row| row.active_skill_id.as_str())
            .collect();
        assert_eq!(twister_rows, vec!["twister"]);
    }

    #[test]
    fn effect_catalog_does_not_let_generic_folder_name_steal_action_match() {
        let activeskills = build_activeskills_with_actions(&[
            ("earthquake", "Earthquake", 0),
            ("companion_bear_slam", "Slam", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["QuakeSlam", "BearCompanionSlam"]);
        let miscanimated = build_miscanimated_bytes(&[(
            "QuakeSlamCrack",
            "Metadata/Effects/Spells/quake_slam/crack.ao",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/quake_slam/crack.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(
            row(&rows, "earthquake").folders,
            vec!["quake_slam".to_string()]
        );
        assert!(!rows
            .iter()
            .any(|row| row.active_skill_id == "companion_bear_slam"));
    }

    #[test]
    fn effect_catalog_prefers_exact_folder_name_over_longer_affix_match() {
        let activeskills = build_activeskills_with_actions(&[
            ("barrage", "Barrage", 0),
            ("oil_barrage", "Oil Barrage", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Barrage", "ElectricSpit"]);
        let paths = effect_paths(&["metadata/effects/spells/barrage/rig.ao"]);

        let rows =
            build_effect_skill_catalog(&activeskills, &actiontypes, None, None, &paths).unwrap();

        assert_eq!(row(&rows, "barrage").folders, vec!["barrage".to_string()]);
        assert!(!rows.iter().any(|row| row.active_skill_id == "oil_barrage"));
    }

    #[test]
    fn effect_catalog_collapses_same_display_tie_deterministically() {
        let activeskills = build_activeskills_with_actions(&[
            ("lightning_strike", "Lightning Strike", 0),
            ("new_lightning_strike", "Lightning Strike", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["LightningStrike", "NewLightningStrike"]);
        let paths = effect_paths(&["metadata/effects/spells/lightning_strike/rig.ao"]);

        let rows =
            build_effect_skill_catalog(&activeskills, &actiontypes, None, None, &paths).unwrap();

        assert_eq!(
            row(&rows, "lightning_strike").folders,
            vec!["lightning_strike".to_string()]
        );
        assert!(!rows
            .iter()
            .any(|row| row.active_skill_id == "new_lightning_strike"));
    }

    #[test]
    fn effect_catalog_keeps_different_display_tie_ambiguous() {
        let activeskills = build_activeskills_with_actions(&[
            ("fireball", "Fireball", 0),
            ("iceblast", "Iceblast", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Fireball", "Iceblast"]);
        let itemvisual = build_item_visual_effect_bytes(&[
            (
                "Skill_Fireball",
                "Metadata/Effects/Spells/supports/shared.ao",
            ),
            (
                "Skill_Iceblast",
                "Metadata/Effects/Spells/supports/shared.ao",
            ),
        ]);
        let paths = effect_paths(&["metadata/effects/spells/supports/shared.ao"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            Some(&itemvisual),
            None,
            &paths,
        )
        .unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_drops_same_confidence_ambiguous_folders() {
        let activeskills = build_activeskills_with_actions(&[
            ("firebolt", "Firebolt", 0),
            ("fireball", "Fireball", 0),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Fireball"]);
        let itemvisual = build_item_visual_effect_bytes(&[(
            "Skill_Fireball",
            "Metadata/Effects/Spells/fire_fireball/epk/buff.epk",
        )]);
        let paths = effect_paths(&["metadata/effects/spells/fire_fireball/epk/buff.epk"]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            Some(&itemvisual),
            None,
            &paths,
        )
        .unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn effect_catalog_prefers_high_confidence_over_medium() {
        let activeskills = build_activeskills_with_actions(&[
            ("firebolt", "Firebolt", 0),
            ("fireball", "Fireball", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["Fireball", "GreaterFireball"]);
        let itemvisual = build_item_visual_effect_bytes(&[(
            "Skill_Fireball",
            "Metadata/Effects/Spells/fire_fireball/epk/buff.epk",
        )]);
        let miscanimated = build_miscanimated_bytes(&[(
            "GreaterFireballExplosion",
            "Metadata/Effects/Spells/fire_fireball/explosion.ao",
        )]);
        let paths = effect_paths(&[
            "metadata/effects/spells/fire_fireball/epk/buff.epk",
            "metadata/effects/spells/fire_fireball/explosion.ao",
        ]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            Some(&itemvisual),
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(
            row(&rows, "firebolt").folders,
            vec!["fire_fireball".to_string()]
        );
        assert!(!rows.iter().any(|row| row.active_skill_id == "fireball"));
    }

    #[test]
    fn effect_catalog_drops_shared_medium_confidence_folders() {
        let activeskills = build_activeskills_with_actions(&[
            ("triggered_a", "Triggered A", 0),
            ("triggered_b", "Triggered B", 1),
        ]);
        let actiontypes = build_actiontypes_bytes(&["TriggeredA", "TriggeredB"]);
        let miscanimated = build_miscanimated_bytes(&[
            (
                "TriggeredAExplosion",
                "Metadata/Effects/Spells/supports/triggered_a.ao",
            ),
            (
                "TriggeredBExplosion",
                "Metadata/Effects/Spells/supports/triggered_b.ao",
            ),
        ]);
        let paths = effect_paths(&[
            "metadata/effects/spells/supports/triggered_a.ao",
            "metadata/effects/spells/supports/triggered_b.ao",
        ]);

        let rows = build_effect_skill_catalog(
            &activeskills,
            &actiontypes,
            None,
            Some(&miscanimated),
            &paths,
        )
        .unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn parse_table_skips_false_marker_inside_row_data() {
        // 3 rows x 16 bytes. Row 0's second field is the heap marker's own
        // bytes (0xBB x8) -- a decoy a naive first-match scan would mistake for
        // the heap boundary. It sits at file offset 12, giving block_len 8 and
        // 8 % 3 != 0, so it's rejected; the real marker is at 4 + 3*16 = 52.
        let mut heap = TEST_ROW_MARKER.to_vec();
        let hello_off = push_utf16(&mut heap, "Hello");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        // row 0: real string offset, then the decoy marker
        bytes.extend_from_slice(&hello_off.to_le_bytes());
        bytes.extend_from_slice(&TEST_ROW_MARKER);
        // rows 1 and 2: empty
        bytes.extend_from_slice(&[0u8; 32]);
        bytes.extend_from_slice(&heap);

        let table = parse_table(&bytes).expect("real marker found past the decoy");
        assert_eq!(table.row_count, 3);
        assert_eq!(table.row_len, 16);
        let first = table.rows().next().unwrap();
        let off = u64::from_le_bytes(first[0..8].try_into().unwrap()) as usize;
        assert_eq!(
            utf16le_string_at(table.heap(), off).as_deref(),
            Some("Hello")
        );
    }

    #[test]
    fn utf16le_string_at_requires_terminator() {
        let mut terminated = Vec::new();
        for unit in "Hi".encode_utf16() {
            terminated.extend_from_slice(&unit.to_le_bytes());
        }
        // A properly NUL-terminated run still resolves.
        let mut with_nul = terminated.clone();
        with_nul.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(utf16le_string_at(&with_nul, 0).as_deref(), Some("Hi"));
        // The same bytes without a terminator run off the end of the heap.
        assert_eq!(utf16le_string_at(&terminated, 0), None);
    }
}
