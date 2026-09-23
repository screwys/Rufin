use std::sync::RwLock;

use spiritless_po::Catalog;

static CATALOGS: RwLock<Vec<Catalog>> = RwLock::new(Vec::new());

/// Installs UTF-8 PO catalogs in language preference order.
///
/// The Android host selects the catalogs from its language preferences and
/// packaged assets. An empty list selects English. Invalid catalogs leave the
/// current translations in place; successful installation also updates readers
/// while the playback service remains alive.
pub fn install_catalogs(catalogs: &[&str]) -> Result<(), String> {
    let catalogs = catalogs
        .iter()
        .enumerate()
        .map(|(index, contents)| {
            let mut catalog = Catalog::new();
            catalog
                .add_str(contents)
                .map_err(|error| format!("could not parse translation catalog {index}: {error}"))?;
            Ok(catalog)
        })
        .collect::<Result<_, String>>()?;
    *CATALOGS
        .write()
        .map_err(|error| format!("could not update translation catalogs: {error}"))? = catalogs;
    Ok(())
}

pub fn tr(message: &str) -> String {
    let catalogs = CATALOGS.read().expect("translation catalogs lock poisoned");
    catalogs
        .iter()
        .find(|catalog| catalog.index().contains_key(message))
        .map_or(message, |catalog| catalog.gettext(message))
        .to_owned()
}

pub fn trn(singular: &str, plural: &str, count: u64) -> String {
    let count = count.min(u64::from(u32::MAX)) as usize;
    let catalogs = CATALOGS.read().expect("translation catalogs lock poisoned");
    catalogs
        .iter()
        .find(|catalog| catalog.index().contains_key(singular))
        .map_or_else(
            || if count == 1 { singular } else { plural },
            |catalog| catalog.ngettext(singular, plural, count),
        )
        .to_owned()
}
