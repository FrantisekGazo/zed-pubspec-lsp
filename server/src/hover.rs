use tower_lsp_server::ls_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::document::Document;
use crate::pubdev::PubDevClient;
use crate::pubspec::DepSource;

pub async fn hover(doc: &Document, pos: Position, pubdev: &PubDevClient) -> Option<Hover> {
    let model = doc.model.as_ref()?;
    let (_, entry) = model.entry_at(pos)?;

    // SDK deps (flutter) aren't on pub.dev; git/path deps often share a name
    // with a published package, so showing pub.dev info for them is useful.
    match entry.source {
        DepSource::Sdk(_) | DepSource::Unknown => return None,
        DepSource::Hosted { .. } | DepSource::Git | DepSource::Path => {}
    }

    let info = pubdev.package_info(&entry.name).await?;

    let mut md = format!("**{}**", info.name);
    if let Some(description) = &info.description {
        md.push_str(" — ");
        md.push_str(description);
    }
    md.push_str(&format!(
        "\n\nLatest: `{}` · [pub.dev]({})",
        info.latest,
        info.pub_dev_url()
    ));
    if info.is_discontinued {
        md.push_str("\n\n⚠️ **Discontinued**");
        if let Some(replacement) = &info.replaced_by {
            md.push_str(&format!(
                " — replaced by [{replacement}]({PUB_DEV}/packages/{replacement})",
                PUB_DEV = crate::pubdev::PUB_DEV_URL
            ));
        }
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: Some(entry.name_range),
    })
}
