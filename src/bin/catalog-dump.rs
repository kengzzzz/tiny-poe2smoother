//! Temporary dev tool: dump the real monster-effect catalog rows.

use anyhow::Result;
use tiny_poe2smoother::bundle::BundleStore;
use tiny_poe2smoother::install::resolve_game_dir;
use tiny_poe2smoother::patches::build_monster_effect_catalog;

fn main() -> Result<()> {
    let game_dir = resolve_game_dir(None)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let rows = build_monster_effect_catalog(&store, &mut index)?;
    eprintln!("rows: {}", rows.len());
    for row in &rows {
        let effective_aliases = row
            .search_aliases
            .iter()
            .chain(&row.derived_context)
            .cloned()
            .collect::<Vec<_>>()
            .join("|");
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.display,
            row.family,
            if row.named { "named" } else { "fallback" },
            row.monster_keys.len(),
            row.monster_keys.join("|"),
            effective_aliases,
            row.derived_context.join("|"),
            row.search_aliases.join("|")
        );
    }
    Ok(())
}
