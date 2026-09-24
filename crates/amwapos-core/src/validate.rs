//! Shared input validation helpers.

use crate::error::{AppError, AppResult};

/// Normalize a scanned or typed barcode. Barcodes are opaque text: leading
/// zeros are preserved, only surrounding whitespace / CR / LF / TAB (which
/// scanners append) is removed. Internal whitespace and control characters
/// are rejected. Never converted to a number.
pub fn barcode(raw: &str) -> AppResult<String> {
    let b = raw.trim_matches(|c: char| c.is_whitespace() || c == '\r' || c == '\n' || c == '\t');
    if b.is_empty() {
        return Err(AppError::validation("The barcode is empty."));
    }
    if b.len() > 64 {
        return Err(AppError::validation("The barcode is longer than 64 characters."));
    }
    if !b.chars().all(|c| c.is_ascii_graphic()) {
        return Err(AppError::validation(
            "The barcode contains invalid characters. Barcodes may contain letters, digits and symbols only.",
        ));
    }
    Ok(b.to_string())
}

pub fn money_non_negative(v: i64, field: &str) -> AppResult<i64> {
    if v < 0 {
        return Err(AppError::validation(format!("{field} cannot be negative.")));
    }
    if v > 1_000_000_000_000 {
        return Err(AppError::validation(format!("{field} is too large.")));
    }
    Ok(v)
}

pub fn qty_positive(v: i64, allow_decimal: bool, field: &str) -> AppResult<i64> {
    if v <= 0 {
        return Err(AppError::validation(format!("{field} must be greater than zero.")));
    }
    if v > 1_000_000_000 {
        return Err(AppError::validation(format!("{field} is too large.")));
    }
    if !allow_decimal && v % crate::money::QTY_SCALE != 0 {
        return Err(AppError::validation(format!("{field} must be a whole number for this product.")));
    }
    Ok(v)
}

pub fn id(v: &str, field: &str) -> AppResult<String> {
    let t = v.trim();
    if t.is_empty() || t.len() > 64 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(AppError::validation(format!("{field} is not a valid identifier.")));
    }
    Ok(t.to_string())
}

pub fn limit(v: Option<i64>, default: i64, max: i64) -> i64 {
    v.unwrap_or(default).clamp(1, max)
}

pub fn offset(v: Option<i64>) -> i64 {
    v.unwrap_or(0).max(0)
}

/// Build a safe FTS5 query: every token becomes a quoted prefix term.
/// Returns None when no searchable token remains.
pub fn fts_query(q: &str) -> Option<String> {
    let terms: Vec<String> =
        q.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).take(8).map(|t| format!("\"{}\"*", t.replace('"', ""))).collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn barcode_rules() {
        assert_eq!(barcode("0001234567890\r\n").unwrap(), "0001234567890");
        assert_eq!(barcode("  6291100001234\t").unwrap(), "6291100001234");
        assert_eq!(barcode("ABC-123/X").unwrap(), "ABC-123/X");
        assert!(barcode("").is_err());
        assert!(barcode("12 34").is_err());
        assert!(barcode("12\u{7}34").is_err());
        assert!(barcode(&"9".repeat(65)).is_err());
    }
    #[test]
    fn fts() {
        assert_eq!(fts_query("coca cola").unwrap(), "\"coca\"* AND \"cola\"*");
        assert_eq!(fts_query("  \"*^ ").as_deref(), None);
        assert_eq!(fts_query("حليب").unwrap(), "\"حليب\"*");
    }
}
