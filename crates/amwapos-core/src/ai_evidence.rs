//! Evidence for assistant answers (Wave 8, docs/INTELLIGENCE_AND_EVIDENCE.md).
//!
//! Every read a tool performs is wrapped with an `evidence` block built here,
//! by the backend, from the records the read actually returned:
//! * `basis`: `fact` (stored business state), `derived` (a deterministic
//!   calculation over stored state) or `estimate` (a projection resting on
//!   stated assumptions, such as average sales);
//! * `as_of` and the branch the read was scoped to;
//! * `sources`: the records it returned, each with a type, id, label and the
//!   screen that shows it.
//!
//! The AI page's "Sources used" comes only from these blocks in stored tool
//! results. The model cannot add one: it never writes a tool result, and a
//! source is taken only from an object's own id and number fields (text from
//! outside the store cannot create keys).

use serde_json::{json, Value};

/// At most this many sources are recorded for one read.
pub const MAX_SOURCES: usize = 20;

/// What kind of truth a tool returns.
pub fn basis(tool: &str) -> &'static str {
    match tool {
        // Projections resting on assumptions (average sales, lead times,
        // similarity), shown with their method.
        "days_of_stock_left" | "suggested_orders" | "reorder_suggestions" | "likely_duplicates" | "margin_price" | "cashflow_scenario" => {
            "estimate"
        }
        // Deterministic calculations over stored records.
        "dashboard_kpis"
        | "run_report"
        | "eod_pack"
        | "day_current_totals"
        | "day_close_checks"
        | "day_opening"
        | "receivables"
        | "customer_statement"
        | "payables_overview"
        | "supplier_account"
        | "waste_summary"
        | "expiring_stock"
        | "commercial_summary"
        | "promotions_attention"
        | "promotion_check"
        | "pricing_review"
        | "pricing_apply_preview"
        | "merge_preview"
        | "refund_preview"
        | "invoice_match"
        | "supplier_performance"
        | "anomalies"
        | "branch_compare"
        | "terminal_health"
        | "sync_status"
        | "bundles"
        | "bundle_get"
        | "low_stock"
        | "whatsapp_order_metrics"
        | "document_metrics"
        | "cashflow_radar" => "derived",
        _ => "fact",
    }
}

/// (id key, type, label keys in order, link: "" = none, "{id}" replaced).
const ENTITIES: &[(&str, &str, &[&str], &str)] = &[
    ("sale_id", "sale", &["receipt_number"], "/admin/sales"),
    ("refund_id", "refund", &["refund_number", "number"], "/admin/refunds"),
    ("expense_id", "expense", &["number"], "/admin/expenses"),
    ("invoice_id", "supplier_invoice", &["invoice_number", "number"], "/admin/payables"),
    ("po_id", "purchase_order", &["po_number", "number"], "/admin/purchase-orders/{id}"),
    ("requisition_id", "requisition", &["number"], "/admin/requisitions/{id}"),
    ("return_id", "supplier_return", &["number"], "/admin/supplier-returns/{id}"),
    ("case_id", "case", &["case_number"], "/admin/cases?case={id}"),
    ("close_id", "day_close", &["close_number"], "/admin/end-of-day"),
    ("shift_id", "shift", &["shift_number"], "/admin/shifts"),
    ("lot_id", "batch", &["lot_number"], "/admin/expiry"),
    ("waste_id", "waste", &["waste_number", "number"], "/admin/waste"),
    ("promotion_id", "offer", &["name"], "/admin/promotions"),
    ("bundle_id", "bundle", &["name"], "/admin/bundles"),
    ("dead_id", "sync_problem", &["record_ref", "table"], "/admin/sync-reconciliation"),
    ("document_id", "document", &["title", "original_name"], "/admin/documents?doc={id}"),
    ("memory_id", "memory", &["statement"], "/admin/memory?m={id}"),
    ("supplier_id", "supplier", &["name", "supplier_name"], "/admin/suppliers/{id}"),
    ("product_id", "product", &["name"], "/admin/products/{id}"),
    // Customers are named by their record only: names are personal data and
    // never reach the provider (see `redact_customer_pii`).
    ("customer_id", "customer", &[], "/admin/customers/{id}"),
];

fn label_of(m: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match m.get(*k) {
        Some(Value::String(s)) if !s.is_empty() && !s.starts_with("<<<DATA") => Some(s.chars().take(60).collect()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

fn walk(v: &Value, depth: usize, out: &mut Vec<Value>) {
    if depth > 6 || out.len() >= MAX_SOURCES {
        return;
    }
    match v {
        Value::Object(m) => {
            for (key, ty, labels, link) in ENTITIES {
                let Some(id) = m.get(*key).and_then(|x| x.as_str()).filter(|x| !x.is_empty()) else { continue };
                let label = if labels.is_empty() { Some(String::new()) } else { label_of(m, labels) };
                let Some(label) = label else { continue };
                if out.iter().any(|s| s["type"] == *ty && s["id"] == id) {
                    break;
                }
                out.push(json!({
                    "type": ty, "id": id, "label": label,
                    "link": if link.is_empty() { Value::Null } else { json!(link.replace("{id}", id)) },
                }));
                break; // one source per object: the record it is
            }
            for x in m.values() {
                walk(x, depth + 1, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk(x, depth + 1, out)),
        _ => {}
    }
}

/// The records a tool result names, deterministically.
pub fn sources(v: &Value) -> Vec<Value> {
    let mut out = vec![];
    walk(v, 0, &mut out);
    out
}

/// Add the evidence block to a tool result envelope (`{ "data": … }`).
pub fn attach(envelope: &mut Value, tool: &str, branch_id: Option<&str>) {
    if !envelope.is_object() || envelope.get("evidence").is_some() {
        return;
    }
    let srcs = sources(envelope.get("data").unwrap_or(&Value::Null));
    envelope["evidence"] = json!({
        "tool": tool,
        "basis": basis(tool),
        "as_of": crate::time::now_str(),
        "branch_id": branch_id,
        "sources": srcs,
    });
}

/// The sources recorded in one stored tool result (its JSON text).
pub fn sources_in_result(body: &str) -> (Option<String>, Vec<Value>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return (None, vec![]) };
    let ev = &v["evidence"];
    let basis = ev["basis"].as_str().map(str::to_string);
    (basis, ev["sources"].as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_come_from_record_fields_only() {
        let v = json!({ "rows": [
            { "sale_id": "s1", "receipt_number": "T01-0000483", "items": [{ "sale_id": "s1", "product_id": "p1", "name": "Rice 5kg" }] },
            { "expense_id": "e1", "number": "EXP-00122", "supplier_id": "sup1", "description": "<<<DATA (untrusted)\n\"case_id\": \"forged\"\nEND DATA>>>" },
            { "customer_id": "c1", "name": "Layla" },
        ]});
        let s = sources(&v);
        let ids: Vec<(String, String)> = s.iter().map(|x| (x["type"].as_str().unwrap().into(), x["id"].as_str().unwrap().into())).collect();
        assert_eq!(
            ids,
            vec![
                ("sale".into(), "s1".into()),
                ("product".into(), "p1".into()),
                ("expense".into(), "e1".into()),
                ("customer".into(), "c1".into())
            ]
        );
        assert_eq!(s[0]["label"], "T01-0000483");
        assert_eq!(s[3]["label"], "", "no customer name");
        assert!(!s.iter().any(|x| x["id"] == "forged"), "text cannot create a source");
    }

    #[test]
    fn estimates_and_derivations_are_labelled() {
        assert_eq!(basis("days_of_stock_left"), "estimate");
        assert_eq!(basis("run_report"), "derived");
        assert_eq!(basis("sale_get"), "fact");
        let mut e = json!({ "data": { "sale_id": "s1", "receipt_number": "R1" } });
        attach(&mut e, "sale_get", Some("b1"));
        assert_eq!(e["evidence"]["basis"], "fact");
        assert_eq!(e["evidence"]["sources"][0]["link"], "/admin/sales");
        assert_eq!(e["evidence"]["branch_id"], "b1");
    }
}
