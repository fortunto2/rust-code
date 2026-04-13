use std::fmt;

/// UTF-8 safe string operations missing from stdlib.
pub trait StrExt {
    /// Truncate to max bytes at char boundary.
    fn trunc(&self, max_bytes: usize) -> &str;

    /// Truncate with ellipsis for display: `"long text…"`.
    fn ellipsis(&self, max_bytes: usize) -> Ellipsis<'_>;

    /// Single-line preview: collapse whitespace + truncate.
    fn oneline(&self, max_bytes: usize) -> String;
}

impl StrExt for str {
    #[inline]
    fn trunc(&self, max_bytes: usize) -> &str {
        &self[..self.floor_char_boundary(max_bytes)]
    }

    #[inline]
    fn ellipsis(&self, max_bytes: usize) -> Ellipsis<'_> {
        Ellipsis(self, max_bytes)
    }

    fn oneline(&self, max_bytes: usize) -> String {
        let flat: String = self.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.trunc(max_bytes).to_string()
    }
}

/// Display wrapper that appends "…" when truncated.
pub struct Ellipsis<'a>(&'a str, usize);

impl fmt::Display for Ellipsis<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.trunc(self.1);
        f.write_str(s)?;
        if s.len() < self.0.len() {
            f.write_str("…")?;
        }
        Ok(())
    }
}
