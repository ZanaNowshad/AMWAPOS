//! The AI contract for one WhatsApp message. The model may classify the
//! intent and pick, for each item phrase, one of the real candidate products
//! we sent; everything else is dropped. It never sets prices, fees, stock or
//! the order's status.

use serde_json::Value;

use super::interpret::Intent;

pub const SCHEMA_HINT: &str = r#"{"intent":"new_order|order_modification|product_question|price_question|availability_question|delivery_address|payment|confirmation|cancellation|support_issue|greeting|spam|unknown","items":[{"text":string,"qty":number|null,"product_id":string|null}]}"#;

#[derive(Debug, Clone, PartialEq)]
pub struct AiItem {
    pub text: String,
    pub qty_milli: Option<i64>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AiOrder {
    pub intent: Option<Intent>,
    pub items: Vec<AiItem>,
    pub rejected: Vec<String>,
}

pub fn validate(v: &Value, allowed: &[String]) -> Result<AiOrder, String> {
    let o = v.as_object().ok_or("the reply is not a JSON object")?;
    let mut rejected = vec![];
    let intent = match o.get("intent").and_then(|x| x.as_str()) {
        None => None,
        Some(s) => match Intent::parse(s) {
            Some(i) => Some(i),
            None => {
                rejected.push(format!("intent '{s}' is not supported"));
                None
            }
        },
    };
    let mut items = vec![];
    match o.get("items") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            for (i, it) in a.iter().take(50).enumerate() {
                let Some(text) = it["text"].as_str().map(|t| t.trim().chars().take(120).collect::<String>()).filter(|t| !t.is_empty())
                else {
                    rejected.push(format!("item {}: no text", i + 1));
                    continue;
                };
                let qty = match &it["qty"] {
                    Value::Null => None,
                    Value::Number(n) => match crate::money::parse_decimal(&n.to_string(), 3) {
                        Ok(q) if q > 0 && q <= 999_000 => Some(q),
                        _ => {
                            rejected.push(format!("item {}: quantity {} is not possible", i + 1, n));
                            None
                        }
                    },
                    _ => {
                        rejected.push(format!("item {}: quantity is not a number", i + 1));
                        None
                    }
                };
                let product_id = match it["product_id"].as_str() {
                    Some(p) if allowed.iter().any(|a| a == p) => Some(p.to_string()),
                    Some(p) => {
                        rejected.push(format!("item {}: product_id {p} was not one of the candidates", i + 1));
                        None
                    }
                    None => None,
                };
                items.push(AiItem { text, qty_milli: qty, product_id });
            }
        }
        Some(_) => return Err("items is not a list".into()),
    }
    Ok(AiOrder { intent, items, rejected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn candidates_only() {
        let r = validate(
            &json!({ "intent": "new_order", "items": [
                { "text": "coke big", "qty": 2, "product_id": "P2" },
                { "text": "milk", "qty": 0, "product_id": "INVENTED" },
                { "text": "", "qty": 1 } ] }),
            &["P1".into(), "P2".into()],
        )
        .unwrap();
        assert_eq!(r.intent, Some(Intent::NewOrder));
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].product_id.as_deref(), Some("P2"));
        assert_eq!(r.items[1].product_id, None);
        assert_eq!(r.items[1].qty_milli, None);
        assert_eq!(r.rejected.len(), 3, "{:?}", r.rejected);
        assert!(validate(&json!({ "intent": "buy_everything" }), &[]).unwrap().intent.is_none());
        assert!(validate(&json!([1]), &[]).is_err());
    }
}
