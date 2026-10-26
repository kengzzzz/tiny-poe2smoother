use super::catalog::{PatchId, PatchOptions};
use super::datc64::{parse_table, utf16le_string_at, with_strings};
use anyhow::{Context, Result, bail};

pub(super) const CLIENT_STRINGS_PATH: &str = "data/balance/clientstrings.datc64";

const ID_COLUMN: usize = 0;
const TEXT_COLUMN: usize = 8;

const AFFIX_LINES: &[(&str, &str)] = &[
    ("ModDescriptionLinePrefix", "P \"{0}\""),
    ("ModDescriptionLineSuffix", "S \"{0}\""),
];
const TIER_LINES: &[(&str, &str)] = &[
    ("ModDescriptionLineTier", " T{0}"),
    ("ModDescriptionLineTierLevel", " T{0} L{1}"),
];

pub(super) fn compact_mod_lines(bytes: &[u8], options: &PatchOptions) -> Result<Vec<u8>> {
    let mut wanted: Vec<(&str, &str)> = Vec::new();
    if options.enabled(PatchId::CompactModLines, "affix") {
        wanted.extend_from_slice(AFFIX_LINES);
    }
    if options.enabled(PatchId::CompactModLines, "tier") {
        wanted.extend_from_slice(TIER_LINES);
    }
    if wanted.is_empty() {
        return Ok(bytes.to_vec());
    }

    let table = parse_table(bytes).context("clientstrings.datc64 is not a readable table")?;
    if table.row_len < TEXT_COLUMN + 8 {
        bail!("clientstrings.datc64 rows are narrower than expected");
    }
    let cell = |row: &[u8], column: usize| {
        let offset = u64::from_le_bytes(row[column..column + 8].try_into().unwrap());
        usize::try_from(offset)
            .ok()
            .and_then(|offset| utf16le_string_at(table.heap(), offset))
    };

    let mut found = 0;
    let mut edits = Vec::new();
    for (index, row) in table.rows().enumerate() {
        let Some(id) = cell(row, ID_COLUMN) else {
            continue;
        };
        let Some(&(_, text)) = wanted.iter().find(|(key, _)| *key == id) else {
            continue;
        };
        found += 1;
        let Some(current) = cell(row, TEXT_COLUMN).filter(|current| current.contains("{0}")) else {
            bail!(
                "clientstrings.datc64 changed layout ({id} has no text column).\n\
                 Verify game files or wait for a tiny-poe2smoother update"
            );
        };
        if current != text {
            edits.push((index, TEXT_COLUMN, text));
        }
    }
    if found == 0 {
        bail!(
            "clientstrings.datc64 has none of the mod description lines.\n\
             Verify game files or wait for a tiny-poe2smoother update"
        );
    }
    if edits.is_empty() {
        return Ok(bytes.to_vec());
    }
    with_strings(bytes, &edits).context("failed to rewrite clientstrings.datc64")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(&str, &str)]) -> Vec<u8> {
        let mut heap = vec![0xbb; 8];
        let mut push = |text: &str| {
            let offset = heap.len() as u64;
            for unit in text.encode_utf16() {
                heap.extend_from_slice(&unit.to_le_bytes());
            }
            heap.extend_from_slice(&[0; 4]);
            offset
        };
        let mut row_block = Vec::new();
        for (id, text) in rows {
            row_block.extend_from_slice(&push(id).to_le_bytes());
            row_block.extend_from_slice(&push(text).to_le_bytes());
            row_block.extend_from_slice(&[0; 4]);
        }
        let mut bytes = (rows.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&row_block);
        bytes.extend_from_slice(&heap);
        bytes
    }

    fn texts(bytes: &[u8]) -> Vec<String> {
        let table = parse_table(bytes).unwrap();
        table
            .rows()
            .map(|row| {
                let offset = u64::from_le_bytes(row[8..16].try_into().unwrap());
                utf16le_string_at(table.heap(), offset as usize).unwrap()
            })
            .collect()
    }

    const ROWS: &[(&str, &str)] = &[
        ("ModDescriptionLinePrefix", "Prefix Modifier \"{0}\""),
        ("ModDescriptionLineCrafted", "Crafted {0}"),
        ("ModDescriptionLineSuffix", "Suffix Modifier \"{0}\""),
        ("ModDescriptionLineTier", " (Tier: {0})"),
        ("ModDescriptionLineTierLevel", " (Tier: {0}, Lvl: {1})"),
    ];

    #[test]
    fn shortens_affix_and_tier_lines_and_leaves_other_rows() {
        let out = compact_mod_lines(&table(ROWS), &PatchOptions::new()).unwrap();
        assert_eq!(
            texts(&out),
            [
                "P \"{0}\"",
                "Crafted {0}",
                "S \"{0}\"",
                " T{0}",
                " T{0} L{1}"
            ]
        );
    }

    #[test]
    fn options_select_which_lines_change() {
        let mut options = PatchOptions::new();
        options.set(PatchId::CompactModLines, "affix", false);
        let out = compact_mod_lines(&table(ROWS), &options).unwrap();
        assert_eq!(texts(&out)[0], "Prefix Modifier \"{0}\"");
        assert_eq!(texts(&out)[3], " T{0}");
    }

    #[test]
    fn already_compact_table_is_returned_unchanged() {
        let bytes = table(ROWS);
        let once = compact_mod_lines(&bytes, &PatchOptions::new()).unwrap();
        let twice = compact_mod_lines(&once, &PatchOptions::new()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn row_whose_text_column_is_not_a_template_is_rejected() {
        let mut bytes = table(ROWS);
        bytes[4 + 8..4 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(compact_mod_lines(&bytes, &PatchOptions::new()).is_err());

        let bytes = table(&[("ModDescriptionLinePrefix", "Prefix Modifier")]);
        assert!(compact_mod_lines(&bytes, &PatchOptions::new()).is_err());
    }

    #[test]
    fn table_without_the_rows_is_an_error() {
        let bytes = table(&[("SomethingElse", "Text")]);
        assert!(compact_mod_lines(&bytes, &PatchOptions::new()).is_err());
    }
}
