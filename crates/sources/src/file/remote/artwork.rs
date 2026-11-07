use crate::SourceResult;
use crate::file::artwork::FolderImage;

/// The supported images the catalog holds directly in the folder `prefix`.
pub(crate) async fn directory_images(
    catalog: &library::Catalog,
    source_id: &str,
    prefix: &str,
) -> SourceResult<Vec<FolderImage>> {
    let mut after = String::new();
    let mut images = Vec::new();
    loop {
        let page = catalog
            .file_artwork_image_page(source_id, prefix, &after)
            .await?;
        if page.is_empty() {
            return Ok(images);
        }
        for (path, revision) in page {
            after.clone_from(&path);
            let uri = url::Url::parse(&path).map_err(|_| crate::SourceError::NotFound)?;
            let relative = percent_encoding::percent_decode_str(uri.path())
                .decode_utf8()
                .map_err(|_| crate::SourceError::NotFound)?;
            let relative = if uri.scheme() == "rufin-document" {
                crate::file::documents::display_path(&relative)
            } else {
                relative.into_owned()
            };
            let name = relative.rsplit('/').next().unwrap_or_default();
            if crate::file::artwork::supported_image(std::path::Path::new(name)) {
                images.push(FolderImage {
                    name: name.to_owned(),
                    path,
                    revision,
                });
            }
        }
    }
}
