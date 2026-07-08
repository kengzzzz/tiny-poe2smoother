use super::targeting::{is_metadata_effect_ext, path_byte_eq, starts_with_path_ci};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const SPELLS_PREFIX: &str = "metadata/effects/spells/";

/// How the `Effects` patch treats one top-level skill folder under
/// `metadata/effects/spells/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectLevel {
    /// Leave every file of the skill untouched (original visuals).
    Full,
    /// Strip nonessential client blocks from `.ao`/`.aoc` (the patch's
    /// long-standing behavior).
    #[default]
    Reduced,
    /// Reduced plus blanking the skill's `.epk`/`.pet`/`.trl` effect data.
    Hidden,
}

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

/// Per-request view over the overrides. `None` (no overrides) keeps target
/// collection and transforms byte-identical to the unfiltered behavior.
#[derive(Debug)]
pub(super) struct EffectsFilter {
    levels: Vec<(String, EffectLevel)>,
}

impl EffectsFilter {
    pub(super) fn new(overrides: &[EffectSkillOverride]) -> Option<Self> {
        let levels: Vec<_> = overrides
            .iter()
            .filter(|entry| entry.level != EffectLevel::Reduced)
            .map(|entry| (entry.folder.clone(), entry.level))
            .collect();
        (!levels.is_empty()).then_some(Self { levels })
    }

    pub(super) fn level_for(&self, path: &str) -> EffectLevel {
        let Some(folder) = spells_folder(path) else {
            return EffectLevel::Reduced;
        };
        self.levels
            .iter()
            .find(|(name, _)| folder_eq(folder, name))
            .map(|(_, level)| *level)
            .unwrap_or_default()
    }

    pub(super) fn has_full(&self) -> bool {
        self.levels
            .iter()
            .any(|(_, level)| *level == EffectLevel::Full)
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

fn folder_eq(folder: &str, name: &str) -> bool {
    folder.len() == name.len()
        && folder
            .bytes()
            .zip(name.bytes())
            .all(|(a, b)| path_byte_eq(a, b))
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

    let mut by_skill: BTreeMap<String, EffectSkillCatalogEntry> = BTreeMap::new();
    for (folder, skill_candidates) in candidates {
        let best = skill_candidates
            .values()
            .map(|candidate| candidate.confidence)
            .max()?;
        let mut best_candidates = skill_candidates
            .values()
            .filter(|candidate| candidate.confidence == best);
        let Some(candidate) = best_candidates.next() else {
            continue;
        };
        if best_candidates.next().is_some() {
            continue;
        }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EvidenceConfidence {
    Medium,
    High,
}

#[derive(Debug)]
struct FolderCandidate {
    active_skill_id: String,
    display: String,
    action_type: String,
    confidence: EvidenceConfidence,
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
                insert_folder_candidate(candidates, &folder, skill, EvidenceConfidence::High);
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
        for skill in skills {
            if !label_key.starts_with(&skill.action_key) {
                continue;
            }
            for folder in &folders {
                insert_folder_candidate(candidates, folder, skill, EvidenceConfidence::Medium);
            }
        }
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
) {
    candidates
        .entry(folder.to_string())
        .or_default()
        .entry(skill.active_skill_id.clone())
        .and_modify(|candidate| {
            candidate.confidence = candidate.confidence.max(confidence);
        })
        .or_insert_with(|| FolderCandidate {
            active_skill_id: skill.active_skill_id.clone(),
            display: skill.display.clone(),
            action_type: skill.action_type.clone(),
            confidence,
        });
}

pub const ACTIVESKILLS_DATC64_PATH: &str = "data/balance/activeskills.datc64";
pub const ACTIONTYPES_DATC64_PATH: &str = "data/balance/actiontypes.datc64";
pub const ITEM_VISUAL_EFFECT_DATC64_PATH: &str = "data/balance/itemvisualeffect.datc64";
const ACTIVESKILLS_ROW_MARKER: [u8; 8] = [0xbb; 8];
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

struct DatTable<'a> {
    row_count: usize,
    row_len: usize,
    row_block: &'a [u8],
    heap: &'a [u8],
}

impl<'a> DatTable<'a> {
    fn rows(&self) -> impl Iterator<Item = &'a [u8]> {
        self.row_block.chunks_exact(self.row_len)
    }

    fn heap(&self) -> &'a [u8] {
        self.heap
    }
}

fn parse_table(bytes: &[u8]) -> Option<DatTable<'_>> {
    let row_count = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    if row_count == 0 {
        return None;
    }
    let marker_pos = bytes
        .windows(ACTIVESKILLS_ROW_MARKER.len())
        .position(|w| w == ACTIVESKILLS_ROW_MARKER)?;
    let row_block = bytes.get(4..marker_pos)?;
    if row_block.len() % row_count != 0 {
        return None;
    }
    let row_len = row_block.len() / row_count;
    if row_len < 8 {
        return None;
    }
    let heap = bytes.get(marker_pos..)?;
    Some(DatTable {
        row_count,
        row_len,
        row_block,
        heap,
    })
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
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let rest = normalized.strip_prefix("metadata/effects/spells/")?;
    rest.split('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

/// Reads a null-terminated UTF-16LE string starting at `offset` in `heap`.
/// `None` for an out-of-range offset, an unterminated run, or invalid
/// UTF-16 -- any of which mean `offset` wasn't really a string pointer.
fn utf16le_string_at(heap: &[u8], offset: usize) -> Option<String> {
    let mut units = Vec::new();
    let mut i = offset;
    while i + 1 < heap.len() {
        let unit = u16::from_le_bytes([heap[i], heap[i + 1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
        i += 2;
        if units.len() > 200 {
            return None;
        }
    }
    if units.is_empty() {
        return None;
    }
    String::from_utf16(&units).ok()
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

/// GGG's translation tooling tags some rows with a leading bracketed
/// annotation (`"[DNT] ..."`, `"[DNT-UNUSED] ..."`, `"[UNUSED] ..."`). The
/// tag is not a reliability signal by itself: `smite`'s only row is
/// `"[DNT-UNUSED] Smite"` despite Smite being a real, current skill, and
/// `herald_of_blood`'s was `"[DNT] herald_of_blood"` despite that skill
/// being live (confirmed against a real install and the wiki). Strip the
/// tag and trust the remainder, except where the remainder itself reads as
/// a dead/placeholder marker rather than a name -- checked against all 113
/// tagged rows in a live install (2026-07-08): exactly 4 matched one of
/// these markers ("DISCONTINUED", "(NOT CURRENTLY USED)", "A placeholder
/// self-buff"), the other 109 were real skill names.
fn clean_display_name(name: &str) -> Option<String> {
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
        assert!(EffectsFilter::new(&[]).is_none());
        // Explicit Reduced entries are meaningless and collapse to None.
        assert!(EffectsFilter::new(&[EffectSkillOverride {
            folder: "arc_02".to_string(),
            level: EffectLevel::Reduced,
        }])
        .is_none());

        let filter = EffectsFilter::new(&[
            EffectSkillOverride {
                folder: "cold_herald_of_ice".to_string(),
                level: EffectLevel::Full,
            },
            EffectSkillOverride {
                folder: "arc_02".to_string(),
                level: EffectLevel::Hidden,
            },
        ])
        .unwrap();
        assert_eq!(
            filter.level_for("Metadata/Effects/Spells/Cold_Herald_Of_Ice/ao/ice_explosion.ao"),
            EffectLevel::Full
        );
        assert_eq!(
            filter.level_for("metadata/effects/spells/arc_02/beam.pet"),
            EffectLevel::Hidden
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
        let mut heap = ACTIVESKILLS_ROW_MARKER.to_vec();
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
        let mut heap = ACTIVESKILLS_ROW_MARKER.to_vec();
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
        let mut heap = ACTIVESKILLS_ROW_MARKER.to_vec();
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

    fn push_utf16(heap: &mut Vec<u8>, s: &str) -> u64 {
        let offset = heap.len() as u64;
        for unit in s.encode_utf16() {
            heap.extend_from_slice(&unit.to_le_bytes());
        }
        heap.extend_from_slice(&0u16.to_le_bytes());
        offset
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

        let rows =
            build_effect_skill_catalog(&activeskills, &actiontypes, None, Some(&miscanimated), &paths)
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
        assert_eq!(rows[0].active_skill_id, "firebolt");
        assert_eq!(rows[0].folders, vec!["fire_fireball".to_string()]);
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
}
