//! A Svelte component's `<script>` blocks, read as TypeScript (F19 Languages).

use crate::extract::{Definition, FileParse, Reference, ReferenceKind, SymbolKind};

pub fn markup(source: &str) -> FileParse {
    let mut parse = FileParse::default();
    let mut definition = Definition::at(
        source,
        0,
        source.len(),
        "default".into(),
        SymbolKind::Component,
    );
    definition.signature = "Svelte component".into();
    parse.definitions.push(definition);
    let mut offset = 0;
    let mut line = 1;
    while let Some(start) = source[offset..].find('<') {
        let start = offset + start;
        line += source[offset..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
        let tail = &source[start..];
        let skipped = ["<!--", "<script", "<style"]
            .into_iter()
            .zip(["-->", "</script>", "</style>"])
            .find(|(open, _)| tail.starts_with(open))
            .map(|(_, close)| tail.find(close).map_or(tail.len(), |end| end + close.len()));
        let length = skipped.unwrap_or_else(|| {
            let name = tail[1..]
                .split(|character: char| {
                    !(character.is_alphanumeric() || character == '_' || character == '.')
                })
                .next()
                .unwrap_or_default();
            if name.starts_with(char::is_uppercase) && !name.contains('.') {
                parse.references.push(Reference::at(
                    name,
                    ReferenceKind::Component,
                    line,
                    start + 1,
                ));
            }
            1
        });
        offset = start + length;
        line += source[start..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
    }
    parse
}

/// Each script block's body: its byte offset in the file, the number of lines before it, and
/// its text.
pub fn scripts(source: &str) -> Vec<(u32, u32, &str)> {
    let mut blocks = Vec::new();
    let mut from = 0usize;
    while let Some(open) = source[from..].find("<script") {
        let tag_start = from + open;
        let Some(tag_end) = source[tag_start..].find('>') else {
            break;
        };
        let body_start = tag_start + tag_end + 1;
        let Some(close) = source[body_start..].find("</script>") else {
            break;
        };
        let body_end = body_start + close;
        let lines_before = source[..body_start].matches('\n').count() as u32;
        blocks.push((
            body_start as u32,
            lines_before,
            &source[body_start..body_end],
        ));
        from = body_end + "</script>".len();
    }
    blocks
}
