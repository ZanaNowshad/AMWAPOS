//! Bahrain addresses: Flat, Building (house), Road, Block, plus a landmark.
//!
//! The parts are kept in their own columns (so they can be edited and
//! searched) and composed into the one-line `address` every other screen,
//! slip and WhatsApp notice already prints. The block number tells the area:
//! the shop's own past drops decide first (most used area for that block),
//! then the block list in Settings → Delivery (editable). Free-text addresses keep
//! working when no parts are given.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AddressParts {
    pub flat: Option<String>,
    pub building: Option<String>,
    pub road: Option<String>,
    pub block: Option<String>,
    /// Anything else the rider needs ("near the mosque", "blue gate").
    pub landmark: Option<String>,
    /// capital | muharraq | northern | southern (optional; never guessed).
    pub governorate: Option<String>,
    /// How to get there ("second gate, ring twice"). Optional.
    pub directions: Option<String>,
}

pub const GOVERNORATES: [&str; 4] = ["capital", "muharraq", "northern", "southern"];

/// An area as it is kept: spaces tidied, and a known area's usual spelling
/// ("JUFAIR " → "Juffair"). Unknown names are kept as typed.
pub fn normalize_area(raw: Option<&str>) -> Option<String> {
    let t = raw?.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return None;
    }
    match crate::customers::area_from_text(&t) {
        Some(name) if crate::customers::strip_area(&t, name).is_empty() => Some(name.to_string()),
        _ => Some(t),
    }
}

/// `address_parts: null` from the UI means "no parts" (serde's `default`
/// alone only covers a missing field).
pub fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn part(v: &Option<String>, label: &str, max: usize) -> AppResult<Option<String>> {
    let t = v.as_deref().map(str::trim).unwrap_or("");
    if t.is_empty() {
        return Ok(None);
    }
    if t.chars().count() > max {
        return Err(AppError::validation(format!("{label} is too long.")));
    }
    Ok(Some(t.to_string()))
}

impl AddressParts {
    /// Trimmed parts; a building is needed once any part is given.
    pub fn cleaned(&self) -> AppResult<AddressParts> {
        let p = AddressParts {
            flat: part(&self.flat, "Flat", 20)?,
            building: part(&self.building, "Building", 20)?,
            road: part(&self.road, "Road", 20)?,
            block: part(&self.block, "Block", 10)?,
            landmark: part(&self.landmark, "Landmark", 150)?,
            governorate: part(&self.governorate, "Governorate", 20)?.map(|g| g.to_lowercase()),
            directions: part(&self.directions, "Directions", 300)?,
        };
        if let Some(g) = &p.governorate {
            if !GOVERNORATES.contains(&g.as_str()) {
                return Err(AppError::validation("Choose the governorate from the list."));
            }
        }
        if let Some(b) = &p.block {
            if !b.chars().all(|c| c.is_ascii_digit()) {
                return Err(AppError::validation("The block is a number, for example 256."));
            }
        }
        if p.is_structured() && p.building.is_none() {
            return Err(AppError::validation("Enter the building or house number."));
        }
        Ok(p)
    }

    /// Any of flat, building, road or block given.
    pub fn is_structured(&self) -> bool {
        self.flat.is_some() || self.building.is_some() || self.road.is_some() || self.block.is_some()
    }

    /// "Flat 12, Bldg 1203, Road 4518, Block 245, near the mosque".
    pub fn line(&self) -> Option<String> {
        if !self.is_structured() {
            return None;
        }
        let mut out = vec![];
        if let Some(v) = &self.flat {
            out.push(format!("Flat {v}"));
        }
        if let Some(v) = &self.building {
            out.push(format!("Bldg {v}"));
        }
        if let Some(v) = &self.road {
            out.push(format!("Road {v}"));
        }
        if let Some(v) = &self.block {
            out.push(format!("Block {v}"));
        }
        if let Some(v) = &self.landmark {
            out.push(v.clone());
        }
        Some(out.join(", "))
    }
}

/// The area for a block: the shop's most used area for it, else the editable
/// block list in Settings → Delivery (`delivery.blocks`).
pub fn area_for_block(c: &Connection, block: &str) -> AppResult<Option<String>> {
    let block = block.trim();
    if block.is_empty() {
        return Ok(None);
    }
    let learned: Option<String> = c
        .query_row(
            "SELECT area FROM (
                SELECT area FROM delivery_orders WHERE block=?1 AND area IS NOT NULL AND area<>''
                UNION ALL
                SELECT area FROM customers WHERE block=?1 AND area IS NOT NULL AND area<>''
             ) GROUP BY area ORDER BY COUNT(*) DESC, area LIMIT 1",
            params![block],
            |r| r.get(0),
        )
        .optional()?;
    if learned.is_some() {
        return Ok(learned);
    }
    let n: u32 = match block.parse() {
        Ok(n) => n,
        Err(_) => return Ok(None),
    };
    let cfg: crate::settings::DeliverySettings = crate::settings::get(c, crate::settings::KEY_DELIVERY)?;
    Ok(cfg.blocks.into_iter().find(|r| (r.from..=r.to).contains(&n)).map(|r| r.area))
}

/// Read the parts stored on a row (`prefix.` is the table alias).
pub fn read_parts(c: &Connection, table: &str, key: &str, id: &str) -> AppResult<AddressParts> {
    let sql = format!("SELECT flat, building, road, block, landmark, governorate, directions FROM {table} WHERE {key}=?1");
    Ok(c.query_row(&sql, [id], |r| {
        Ok(AddressParts {
            flat: r.get(0)?,
            building: r.get(1)?,
            road: r.get(2)?,
            block: r.get(3)?,
            landmark: r.get(4)?,
            governorate: r.get(5)?,
            directions: r.get(6)?,
        })
    })
    .optional()?
    .unwrap_or_default())
}

/// Store the parts on a row (clears them when `p` is not structured;
/// governorate and directions are kept either way).
pub fn write_parts(c: &Connection, table: &str, key: &str, id: &str, p: &AddressParts) -> AppResult<()> {
    let (g, d) = (p.governorate.clone(), p.directions.clone());
    let p = if p.is_structured() { p.clone() } else { AddressParts::default() };
    c.execute(
        &format!("UPDATE {table} SET flat=?2, building=?3, road=?4, block=?5, landmark=?6, governorate=?7, directions=?8 WHERE {key}=?1"),
        params![id, p.flat, p.building, p.road, p.block, p.landmark, g, d],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_one_line_and_needs_a_building() {
        let p = AddressParts {
            flat: Some(" 12 ".into()),
            building: Some("1203".into()),
            road: Some("4518".into()),
            block: Some("245".into()),
            landmark: Some("near the mosque".into()),
            ..Default::default()
        }
        .cleaned()
        .unwrap();
        assert_eq!(p.line().unwrap(), "Flat 12, Bldg 1203, Road 4518, Block 245, near the mosque");
        let no_bldg = AddressParts { road: Some("1".into()), ..Default::default() };
        assert!(no_bldg.cleaned().is_err());
        let bad_block = AddressParts { building: Some("1".into()), block: Some("2x".into()), ..Default::default() };
        assert!(bad_block.cleaned().is_err());
        let only_landmark = AddressParts { landmark: Some("blue gate".into()), ..Default::default() }.cleaned().unwrap();
        assert!(only_landmark.line().is_none());
        let g = AddressParts { building: Some("1".into()), governorate: Some("Capital".into()), ..Default::default() }.cleaned().unwrap();
        assert_eq!(g.governorate.as_deref(), Some("capital"));
        assert!(AddressParts { governorate: Some("west".into()), ..Default::default() }.cleaned().is_err());
    }

    #[test]
    fn areas_are_normalized_not_guessed() {
        assert_eq!(normalize_area(Some("  JUFAIR ")).as_deref(), Some("Juffair"));
        assert_eq!(normalize_area(Some("الجفير")).as_deref(), Some("Juffair"));
        assert_eq!(normalize_area(Some("Near   Juffair mall")).as_deref(), Some("Near Juffair mall"));
        assert_eq!(normalize_area(Some("Zinj")).as_deref(), Some("Zinj"));
        assert_eq!(normalize_area(Some("  ")), None);
    }
}
