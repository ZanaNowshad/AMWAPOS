//! Delivery fee from the shop's configured zones (Settings → Delivery). The
//! fee is never invented: no zone (or not enough address) → unresolved.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::address::AddressParts;
use crate::error::AppResult;
use crate::settings::{DeliverySettings, DeliveryZone, KEY_DELIVERY};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZoneFee {
    /// resolved | unresolved | not_applicable
    pub state: String,
    pub zone_id: Option<String>,
    pub zone_name: Option<String>,
    pub area: Option<String>,
    pub fee_minor: Option<i64>,
    pub reason: String,
}

impl ZoneFee {
    fn unresolved(reason: impl Into<String>, area: Option<String>) -> Self {
        ZoneFee { state: "unresolved".into(), zone_id: None, zone_name: None, area, fee_minor: None, reason: reason.into() }
    }
}

pub fn zones(c: &Connection) -> AppResult<Vec<DeliveryZone>> {
    let d: DeliverySettings = crate::settings::get(c, KEY_DELIVERY)?;
    Ok(d.zones.into_iter().filter(|z| z.active).collect())
}

fn fee(z: &DeliveryZone, subtotal: i64) -> i64 {
    match z.free_over_minor {
        Some(f) if subtotal >= f => 0,
        _ => z.fee_minor,
    }
}

/// The zone and fee for an address. `area_text` = an area name the customer
/// wrote; `pinned_zone` = a zone a person chose.
pub fn resolve(
    c: &Connection,
    parts: &AddressParts,
    area_text: Option<&str>,
    pinned_zone: Option<&str>,
    subtotal: i64,
) -> AppResult<ZoneFee> {
    let list = zones(c)?;
    if list.is_empty() {
        return Ok(ZoneFee::unresolved("No delivery zones are set up (Settings → Delivery).", None));
    }
    if let Some(zid) = pinned_zone {
        if let Some(z) = list.iter().find(|z| z.zone_id == zid) {
            return Ok(ZoneFee {
                state: "resolved".into(),
                zone_id: Some(z.zone_id.clone()),
                zone_name: Some(z.name.clone()),
                area: None,
                fee_minor: Some(fee(z, subtotal)),
                reason: "Zone chosen by staff.".into(),
            });
        }
    }
    let area = match (&parts.block, area_text) {
        (Some(b), _) => crate::address::area_for_block(c, b)?,
        (None, Some(a)) => Some(a.to_string()),
        _ => None,
    };
    if let Some(b) = parts.block.as_deref().and_then(|b| b.parse::<u32>().ok()) {
        if let Some(z) = list.iter().find(|z| z.blocks.iter().any(|r| (r.from..=r.to).contains(&b))) {
            return Ok(ZoneFee {
                state: "resolved".into(),
                zone_id: Some(z.zone_id.clone()),
                zone_name: Some(z.name.clone()),
                area,
                fee_minor: Some(fee(z, subtotal)),
                reason: format!("Block {b} is in zone {}.", z.name),
            });
        }
    }
    if let Some(a) = &area {
        let al = a.to_lowercase();
        if let Some(z) = list.iter().find(|z| z.areas.iter().any(|x| x.to_lowercase() == al)) {
            return Ok(ZoneFee {
                state: "resolved".into(),
                zone_id: Some(z.zone_id.clone()),
                zone_name: Some(z.name.clone()),
                area: Some(a.clone()),
                fee_minor: Some(fee(z, subtotal)),
                reason: format!("{a} is in zone {}.", z.name),
            });
        }
        return Ok(ZoneFee::unresolved(format!("{a} is not in any delivery zone."), area));
    }
    Ok(ZoneFee::unresolved("The address has no block or area to find the zone.", None))
}
