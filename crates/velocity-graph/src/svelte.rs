//! A Svelte component's `<script>` blocks, read as TypeScript (F19 Languages).

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
