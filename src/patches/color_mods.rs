//! Color Mods patch: colorize map-modifier text by annotating stat
//! description files (`data/statdescriptions/**/*.csd`).
//!
//! A `.csd` file is UTF-16LE text made of `description` blocks: a header line
//! listing stat ids, then per-language sections of quoted display-text
//! variants. Wrapping a variant in `"<rgb(r,g,b)>{{text}}"` makes the game
//! render it in that color. Approach and the default mod->color table are

use super::text::{decode_utf16, encode_utf16_bom};
use aho_corasick::AhoCorasick;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorModEntry {
    pub stat_id: String,
    pub color: [u8; 3],
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct StatCatalogEntry {
    pub stat_id: String,
    pub text: String,
}

pub const COLOR_RED: [u8; 3] = [209, 46, 46];
pub const COLOR_GREEN: [u8; 3] = [74, 230, 58];
pub const COLOR_BLUE: [u8; 3] = [14, 186, 255];
pub const COLOR_YELLOW: [u8; 3] = [255, 204, 27];
pub const COLOR_PINK: [u8; 3] = [227, 158, 255];

pub const PRESET_COLORS: [(&str, [u8; 3]); 5] = [
    ("red", COLOR_RED),
    ("green", COLOR_GREEN),
    ("yellow", COLOR_YELLOW),
    ("blue", COLOR_BLUE),
    ("pink", COLOR_PINK),
];

static DEFAULT_COLOR_MODS: &[(&str, [u8; 3])] = &[
    ("map_monsters_%_all_damage_to_gain_as_fire", COLOR_RED),
    ("map_monsters_%_physical_damage_to_gain_as_fire", COLOR_RED),
    ("map_monsters_%_all_damage_to_gain_as_cold", COLOR_RED),
    ("map_monsters_%_physical_damage_to_gain_as_cold", COLOR_RED),
    ("map_monsters_%_all_damage_to_gain_as_lightning", COLOR_RED),
    (
        "map_monsters_%_physical_damage_to_gain_as_lightning",
        COLOR_RED,
    ),
    ("map_monsters_%_all_damage_to_gain_as_chaos", COLOR_RED),
    ("map_monsters_%_physical_damage_to_gain_as_chaos", COLOR_RED),
    ("map_monsters_damage_+%", COLOR_RED),
    ("map_monster_damage_+%_final_from_deadly_atlas", COLOR_RED),
    ("map_monster_damage_+%_final_from_vital_atlas", COLOR_RED),
    ("map_monsters_attack_speed_+%", COLOR_RED),
    ("map_monsters_cast_speed_+%", COLOR_RED),
    ("map_monsters_movement_speed_+%", COLOR_RED),
    ("map_monsters_critical_strike_chance_+%", COLOR_RED),
    ("map_monsters_critical_strike_multiplier_+", COLOR_RED),
    ("map_monsters_chance_to_poison_on_hit_%", COLOR_RED),
    ("map_monsters_chance_to_inflict_bleeding_%", COLOR_RED),
    ("map_packs_fire_projectiles", COLOR_RED),
    ("map_monsters_penetrate_elemental_resistances_%", COLOR_RED),
    ("map_monsters_elemental_ailment_chance_+%", COLOR_RED),
    ("map_additional_player_maximum_resistances_%", COLOR_RED),
    ("map_player_life_and_es_recovery_speed_+%_final", COLOR_RED),
    ("map_monsters_life_+%", COLOR_GREEN),
    ("monster_life_+%_final_from_map", COLOR_GREEN),
    ("map_heist_monster_life_+%_final_from_sextant", COLOR_GREEN),
    (
        "map_monsters_armour_break_physical_damage_%_dealt_as_armour_break",
        COLOR_GREEN,
    ),
    ("map_monsters_accuracy_rating_+%", COLOR_GREEN),
    ("map_monsters_hit_damage_stun_multiplier_+%", COLOR_GREEN),
    ("map_monsters_additional_fire_resistance", COLOR_GREEN),
    ("map_monsters_additional_cold_resistance", COLOR_GREEN),
    ("map_monsters_additional_lightning_resistance", COLOR_GREEN),
    ("map_monsters_additional_chaos_resistance", COLOR_GREEN),
    ("map_monsters_ailment_threshold_+%", COLOR_GREEN),
    ("map_monsters_stun_threshold_+%", COLOR_GREEN),
    (
        "map_base_ground_fire_damage_to_deal_per_10_seconds",
        COLOR_GREEN,
    ),
    (
        "map_base_ground_fire_damage_to_deal_per_minute",
        COLOR_GREEN,
    ),
    ("map_ground_ice_base_magnitude", COLOR_GREEN),
    ("map_ground_ice", COLOR_GREEN),
    ("map_ground_lightning", COLOR_GREEN),
    ("map_ground_lightning_base_magnitude", COLOR_GREEN),
    ("flask_charges_gained_+%", COLOR_GREEN),
    ("map_players_cannot_gain_flask_charges", COLOR_GREEN),
    ("map_player_cooldown_speed_+%_final", COLOR_GREEN),
    (
        "map_monsters_base_self_critical_strike_multiplier_-%",
        COLOR_GREEN,
    ),
    ("map_monsters_curse_effect_on_self_+%_final", COLOR_GREEN),
    ("map_monsters_curse_effect_+%", COLOR_GREEN),
    ("map_monster_potency_+%_final_from_map", COLOR_YELLOW),
    ("map_monster_potency_+%", COLOR_YELLOW),
    ("map_item_drop_rarity_+%_final_from_map", COLOR_YELLOW),
    ("map_item_drop_rarity_+%", COLOR_YELLOW),
    ("map_pack_size_+%_final_from_map", COLOR_YELLOW),
    ("map_pack_size_+%", COLOR_YELLOW),
    (
        "map_number_of_magic_and_rare_packs_+%_final_and_rare_monster_modifiers_chance_+%_final_from_map",
        COLOR_YELLOW,
    ),
    (
        "map_number_of_magic_and_rare_packs_+%_and_rare_monster_modifiers_chance_%",
        COLOR_YELLOW,
    ),
    ("map_number_of_magic_packs_+%", COLOR_YELLOW),
    ("map_number_of_rare_packs_+%", COLOR_YELLOW),
    ("map_rare_monster_modifiers_chance_%", COLOR_YELLOW),
    ("map_map_item_drop_chance_+%_final_from_map", COLOR_BLUE),
    ("map_map_item_drop_chance_+%", COLOR_BLUE),
];

pub fn default_color_mods() -> Vec<ColorModEntry> {
    DEFAULT_COLOR_MODS
        .iter()
        .map(|(stat_id, color)| ColorModEntry {
            stat_id: (*stat_id).to_string(),
            color: *color,
            enabled: true,
        })
        .collect()
}

/// Saved entries win (user edits preserved); defaults the user has never seen
/// (added in a newer app version) are appended.
pub fn merge_with_defaults(mut saved: Vec<ColorModEntry>) -> Vec<ColorModEntry> {
    let have: std::collections::HashSet<String> =
        saved.iter().map(|entry| entry.stat_id.clone()).collect();
    saved.extend(
        default_color_mods()
            .into_iter()
            .filter(|entry| !have.contains(&entry.stat_id)),
    );
    saved
}

/// Precomputed lookup shared across the rayon transform tasks: enabled
/// stat id -> color, plus an aho-corasick prefilter over the ids encoded as
/// UTF-16LE so files that cannot contain a match are skipped without the
/// (expensive) full decode of e.g. the 30 MB stat_descriptions.csd.
#[derive(Debug)]
pub(super) struct ColorMatcher {
    colors: HashMap<String, [u8; 3]>,
    prefilter: AhoCorasick,
}

impl ColorMatcher {
    /// `None` when no entry is enabled (the patch is then a no-op).
    pub(super) fn new(entries: &[ColorModEntry]) -> Option<Self> {
        let colors: HashMap<String, [u8; 3]> = entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| (entry.stat_id.clone(), entry.color))
            .collect();
        if colors.is_empty() {
            return None;
        }
        let needles: Vec<Vec<u8>> = colors
            .keys()
            .map(|id| {
                id.encode_utf16()
                    .flat_map(|unit| unit.to_le_bytes())
                    .collect()
            })
            .collect();
        let prefilter = AhoCorasick::new(&needles).ok()?;
        Some(Self { colors, prefilter })
    }

    pub(super) fn colorize(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if self.prefilter.find(bytes).is_none() {
            return Ok(bytes.to_vec());
        }
        let text = decode_utf16(bytes)?;
        let mut lines: Vec<Cow<'_, str>> = text.split("\r\n").map(Cow::Borrowed).collect();
        let mut changed = false;
        let mut state = State::Seeking;

        for line in lines.iter_mut() {
            // A new block resets the machine from any state. (`no_description`
            // lines do not start with "description", so they never match.)
            if line.starts_with("description") {
                state = State::Header;
                continue;
            }
            match state {
                State::Seeking => {}
                State::Header => {
                    let mut tokens = line.split_whitespace();
                    let _count = tokens.next();
                    state = match tokens.find_map(|id| self.colors.get(id)) {
                        Some(&color) => State::Data { color },
                        None => State::Seeking,
                    };
                }
                State::Data { color } => {
                    if let Some(count) = first_token_count(line) {
                        if count > 0 {
                            state = State::Writing {
                                color,
                                remaining: count,
                            };
                        }
                    }
                }
                State::Writing {
                    color,
                    ref mut remaining,
                } => {
                    if let Some(annotated) = annotate_variant_line(line, color) {
                        *line = Cow::Owned(annotated);
                        changed = true;
                    }
                    *remaining -= 1;
                    if *remaining == 0 {
                        state = State::Data { color };
                    }
                }
            }
        }

        if !changed {
            return Ok(bytes.to_vec());
        }
        Ok(encode_utf16_bom(&lines.join("\r\n")))
    }
}

enum State {
    Seeking,
    Header,
    Data { color: [u8; 3] },
    Writing { color: [u8; 3], remaining: usize },
}

fn first_token_count(line: &str) -> Option<usize> {
    line.trim_start_matches('\t')
        .split(' ')
        .next()?
        .parse()
        .ok()
}

fn annotate_variant_line(line: &str, color: [u8; 3]) -> Option<String> {
    if line.contains('<') {
        return None;
    }
    let open = line.find('"')?;
    let inner_len = line[open + 1..].find('"')?;
    let inner = &line[open + 1..open + 1 + inner_len];
    use std::fmt::Write;
    let [r, g, b] = color;
    let mut out = String::with_capacity(line.len() + 24);
    out.push_str(&line[..open]);
    out.push('"');
    let _ = write!(out, "<rgb({r},{g},{b})>");
    out.push_str("{{");
    out.push_str(inner);
    out.push_str("}}");
    out.push('"');
    out.push_str(&line[open + 1 + inner_len + 1..]);
    Some(out)
}

pub fn parse_stat_catalog(text: &str) -> Vec<StatCatalogEntry> {
    enum ParseState {
        Seeking,
        Header,
        AwaitCount(Vec<String>),
        AwaitText(Vec<String>),
    }

    let mut out = Vec::new();
    let mut state = ParseState::Seeking;
    for line in text.split("\r\n") {
        if line.starts_with("description") {
            state = ParseState::Header;
            continue;
        }
        state = match state {
            ParseState::Seeking => ParseState::Seeking,
            ParseState::Header => {
                let mut tokens = line.split_whitespace();
                let _count = tokens.next();
                let ids: Vec<String> = tokens.map(str::to_string).collect();
                if ids.is_empty() {
                    ParseState::Seeking
                } else {
                    ParseState::AwaitCount(ids)
                }
            }
            ParseState::AwaitCount(ids) => {
                if first_token_count(line).is_some() {
                    ParseState::AwaitText(ids)
                } else {
                    // `lang` or malformed: emit ids without text.
                    emit_catalog_entries(&mut out, ids, "");
                    ParseState::Seeking
                }
            }
            ParseState::AwaitText(ids) => {
                let text = first_quoted(line).unwrap_or("");
                emit_catalog_entries(&mut out, ids, text);
                ParseState::Seeking
            }
        };
    }
    out
}

fn emit_catalog_entries(out: &mut Vec<StatCatalogEntry>, ids: Vec<String>, text: &str) {
    for stat_id in ids {
        out.push(StatCatalogEntry {
            stat_id,
            text: text.to_string(),
        });
    }
}

fn first_quoted(line: &str) -> Option<&str> {
    let open = line.find('"')?;
    let len = line[open + 1..].find('"')?;
    Some(&line[open + 1..open + 1 + len])
}

/// Human-readable form of a raw `.csd` display string, matching what the
/// game renders: `[Tag|Shown]` markup collapses to `Shown`, `[Tag]` to
/// `Tag`, `{0}`-style value placeholders become `#`, and literal `\n`
/// escapes become spaces. The editor uses this for row labels and search,
/// so queries match the in-game wording rather than raw markup.
pub fn display_stat_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find(['[', '{', '\\']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if let Some(inner) = tail.strip_prefix('[').and_then(|t| t.split_once(']')) {
            let (markup, after) = inner;
            out.push_str(markup.rsplit('|').next().unwrap_or(markup));
            rest = after;
        } else if let Some((_, after)) = tail.strip_prefix('{').and_then(|t| t.split_once('}')) {
            out.push('#');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("\\n") {
            out.push(' ');
            rest = after;
        } else {
            // Unmatched opener; emit it verbatim (all three are ASCII).
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    // Collapse whitespace runs left over from `\n` replacement.
    if out.contains("  ") {
        out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(entries: &[(&str, [u8; 3])]) -> ColorMatcher {
        let entries: Vec<ColorModEntry> = entries
            .iter()
            .map(|(id, color)| ColorModEntry {
                stat_id: (*id).to_string(),
                color: *color,
                enabled: true,
            })
            .collect();
        ColorMatcher::new(&entries).unwrap()
    }

    const FIXTURE: &str = "description\r\n\
\t2 map_monsters_damage_+% map_monsters_life_+%\r\n\
\t2\r\n\
\t\t#|99 \"{0}% increased Monster Damage\"\r\n\
\t\t100|# \"Monsters deal extreme Damage\" flag1\r\n\
\tlang \"Portuguese\"\r\n\
\t1\r\n\
\t\t# \"Dano de monstro aumentado em {0}%\"\r\n\
\r\n\
description\r\n\
\t1 unrelated_stat\r\n\
\t1\r\n\
\t\t# \"Nothing to see\"\r\n";

    #[test]
    fn colorize_annotates_all_variants_and_languages_of_matched_blocks() {
        let m = matcher(&[("map_monsters_damage_+%", COLOR_RED)]);
        let out = m.colorize(&encode_utf16_bom(FIXTURE)).unwrap();
        let text = decode_utf16(&out).unwrap();

        assert!(text.contains("\t\t#|99 \"<rgb(209,46,46)>{{{0}% increased Monster Damage}}\""));
        assert!(
            text.contains("\t\t100|# \"<rgb(209,46,46)>{{Monsters deal extreme Damage}}\" flag1")
        );
        assert!(text.contains("\t\t# \"<rgb(209,46,46)>{{Dano de monstro aumentado em {0}%}}\""));
        // Non-matching block untouched.
        assert!(text.contains("\t\t# \"Nothing to see\""));
        // Output round-trips as BOM + UTF-16LE.
        assert_eq!(&out[..2], &[0xff, 0xfe]);
    }

    #[test]
    fn colorize_matches_non_first_header_ids() {
        let m = matcher(&[("map_monsters_life_+%", COLOR_GREEN)]);
        let out = m.colorize(&encode_utf16_bom(FIXTURE)).unwrap();
        let text = decode_utf16(&out).unwrap();

        assert!(text.contains("<rgb(74,230,58)>{{{0}% increased Monster Damage}}"));
    }

    #[test]
    fn colorize_returns_input_bytes_when_nothing_matches() {
        let input = encode_utf16_bom(FIXTURE);
        let m = matcher(&[("some_other_stat", COLOR_RED)]);
        assert_eq!(m.colorize(&input).unwrap(), input);
    }

    #[test]
    fn matcher_is_none_when_no_entry_is_enabled() {
        let entries = vec![ColorModEntry {
            stat_id: "map_monsters_damage_+%".to_string(),
            color: COLOR_RED,
            enabled: false,
        }];
        assert!(ColorMatcher::new(&entries).is_none());
    }

    #[test]
    fn variant_lines_containing_markup_are_left_alone() {
        let fixture = "description\r\n\
\t1 map_monsters_damage_+%\r\n\
\t1\r\n\
\t\t# \"already <rgb(1,2,3)>{{marked}}\"\r\n";
        let input = encode_utf16_bom(fixture);
        let m = matcher(&[("map_monsters_damage_+%", COLOR_RED)]);
        assert_eq!(m.colorize(&input).unwrap(), input);
    }

    #[test]
    fn description_line_mid_section_resets_the_machine() {
        // Malformed count larger than the actual variant lines must not bleed
        // into the next block.
        let fixture = "description\r\n\
\t1 map_monsters_damage_+%\r\n\
\t5\r\n\
\t\t# \"only one variant\"\r\n\
description\r\n\
\t1 unrelated_stat\r\n\
\t1\r\n\
\t\t# \"must stay plain\"\r\n";
        let m = matcher(&[("map_monsters_damage_+%", COLOR_RED)]);
        let out = m.colorize(&encode_utf16_bom(fixture)).unwrap();
        let text = decode_utf16(&out).unwrap();

        assert!(text.contains("{{only one variant}}"));
        assert!(text.contains("\t\t# \"must stay plain\""));
    }

    #[test]
    fn catalog_parse_extracts_ids_with_english_text_only() {
        let entries = parse_stat_catalog(FIXTURE);
        let ids: Vec<&str> = entries.iter().map(|e| e.stat_id.as_str()).collect();

        assert_eq!(
            ids,
            [
                "map_monsters_damage_+%",
                "map_monsters_life_+%",
                "unrelated_stat"
            ]
        );
        assert_eq!(entries[0].text, "{0}% increased Monster Damage");
        assert_eq!(entries[1].text, "{0}% increased Monster Damage");
        assert_eq!(entries[2].text, "Nothing to see");
        // Portuguese text never leaks into the catalog.
        assert!(entries.iter().all(|e| !e.text.contains("Dano")));
    }

    #[test]
    fn merge_preserves_user_edits_and_appends_new_defaults() {
        let saved = vec![
            ColorModEntry {
                stat_id: "map_monsters_damage_+%".to_string(),
                color: COLOR_PINK,
                enabled: false,
            },
            ColorModEntry {
                stat_id: "my_custom_stat".to_string(),
                color: COLOR_BLUE,
                enabled: true,
            },
        ];
        let merged = merge_with_defaults(saved);

        let damage = merged
            .iter()
            .find(|e| e.stat_id == "map_monsters_damage_+%")
            .unwrap();
        assert_eq!(damage.color, COLOR_PINK);
        assert!(!damage.enabled);
        assert!(merged.iter().any(|e| e.stat_id == "my_custom_stat"));
        // Every default id is present exactly once.
        for entry in default_color_mods() {
            assert_eq!(
                merged.iter().filter(|e| e.stat_id == entry.stat_id).count(),
                1,
                "{}",
                entry.stat_id
            );
        }
    }

    #[test]
    fn display_stat_text_renders_like_the_game() {
        assert_eq!(
            display_stat_text(
                "[ContainsRitual|Ritual] Favours in Map have {0}% increased chance to be [Omen|Omens]"
            ),
            "Ritual Favours in Map have #% increased chance to be Omens"
        );
        assert_eq!(
            display_stat_text("{0}% of [Physical] damage causes [BloodLoss|Blood Loss]"),
            "#% of Physical damage causes Blood Loss"
        );
        assert_eq!(
            display_stat_text("Shrouded in Darkness \\nContains Omen Altars"),
            "Shrouded in Darkness Contains Omen Altars"
        );
        // Unmatched openers pass through verbatim.
        assert_eq!(
            display_stat_text("broken [markup and {value"),
            "broken [markup and {value"
        );
        assert_eq!(display_stat_text("plain text"), "plain text");
    }
}
