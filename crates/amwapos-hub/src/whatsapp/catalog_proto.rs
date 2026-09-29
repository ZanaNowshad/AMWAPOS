//! WhatsApp Business catalogue stanzas (`xmlns="w:biz:catalog"`), sent as
//! ordinary IQs over the linked session by `RustWhatsAppAdapter`. These are
//! the same stanzas WhatsApp Web / the `Baileys` library use for a Business
//! account's own catalogue (`product_catalog_add`, `product_catalog_edit`,
//! `product_catalog_delete`, `product_catalog` read). There is no Meta Graph
//! or Cloud API here. Collections have no write stanza in this set, so they
//! are not written.
//!
//! Builders and parsers are pure (unit-tested below). Everything coming back
//! from WhatsApp is treated as untrusted: ids are validated, text is bounded.

use whatsapp_rust::wacore_binary::builder::NodeBuilder;
use whatsapp_rust::wacore_binary::{Jid, Node, NodeContent, NodeContentRef, NodeRef};

use super::adapter::{CatalogProduct, RemoteProduct};

pub const NS: &str = "w:biz:catalog";
pub const ADD: &str = "product_catalog_add";
pub const EDIT: &str = "product_catalog_edit";
pub const DELETE: &str = "product_catalog_delete";

/// Remote ids are opaque server strings (digits in practice): accepted only
/// when short and plain.
pub fn valid_remote_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn text_node(tag: &'static str, s: &str) -> Node {
    NodeBuilder::new(tag).bytes(s.as_bytes().to_vec()).build()
}

/// `<product is_hidden=…><id/>?<name/><description/>?<retailer_id/>
///  <media><image><url/></image></media>?<price/>?<currency/></product>`
pub fn product_node(remote_id: Option<&str>, p: &CatalogProduct) -> Node {
    let mut children = vec![];
    if let Some(id) = remote_id {
        children.push(text_node("id", id));
    }
    children.push(text_node("name", &p.name));
    if let Some(d) = &p.description {
        children.push(text_node("description", d));
    }
    if !p.retailer_id.is_empty() {
        children.push(text_node("retailer_id", &p.retailer_id));
    }
    if let Some(url) = &p.image_url {
        children.push(NodeBuilder::new("media").children([NodeBuilder::new("image").children([text_node("url", url)]).build()]).build());
    }
    if let Some(price) = p.price_1000 {
        children.push(text_node("price", &price.to_string()));
    }
    children.push(text_node("currency", &p.currency));
    NodeBuilder::new("product").attr("is_hidden", if p.hidden { "true" } else { "false" }).children(children).build()
}

/// `<product_catalog_add|edit v="1"><product/><width>100</width><height>100</height></…>`
pub fn write_content(op: &'static str, product: Node) -> NodeContent {
    NodeContent::Nodes(vec![NodeBuilder::new(op)
        .attr("v", "1")
        .children([
            product,
            NodeBuilder::new("width").string_content("100").build(),
            NodeBuilder::new("height").string_content("100").build(),
        ])
        .build()])
}

pub fn delete_content(ids: &[String]) -> NodeContent {
    NodeContent::Nodes(vec![NodeBuilder::new(DELETE)
        .attr("v", "1")
        .children(ids.iter().map(|id| NodeBuilder::new("product").children([text_node("id", id)]).build()))
        .build()])
}

/// Read one page of `jid`'s catalogue.
pub fn list_content(jid: &Jid, limit: u32, cursor: Option<&str>) -> NodeContent {
    let mut params = vec![text_node("limit", &limit.to_string()), text_node("width", "100"), text_node("height", "100")];
    if let Some(c) = cursor {
        params.push(text_node("after", c));
    }
    NodeContent::Nodes(vec![NodeBuilder::new("product_catalog")
        .attr("jid", jid.to_string())
        .attr("allow_shop_source", "true")
        .children(params)
        .build()])
}

fn text(n: &NodeRef<'_>, tag: &str, max: usize) -> Option<String> {
    let c = n.get_optional_child(tag)?;
    let s = match c.content.as_ref()? {
        NodeContentRef::String(s) => s.to_string(),
        NodeContentRef::Bytes(b) => std::str::from_utf8(b).ok()?.to_string(),
        NodeContentRef::Nodes(_) => return None,
    };
    let s = s.trim().to_string();
    (!s.is_empty()).then(|| s.chars().take(max).collect())
}

/// A `<product>` element, or None when it has no valid id.
pub fn parse_product(n: &NodeRef<'_>) -> Option<RemoteProduct> {
    // Read one character past the limit: an over-long id is refused, never
    // truncated into a different id.
    let id = text(n, "id", 65).filter(|i| valid_remote_id(i))?;
    let hidden = n.get_attr("is_hidden").map(|v| v.as_str() == "true").unwrap_or(false);
    Some(RemoteProduct { id, retailer_id: text(n, "retailer_id", 200), name: text(n, "name", 500), hidden })
}

/// The product echoed back by an add / edit.
pub fn parse_write_reply(resp: &NodeRef<'_>, op: &str) -> Option<RemoteProduct> {
    parse_product(resp.get_optional_child(op)?.get_optional_child("product")?)
}

pub fn parse_list(resp: &NodeRef<'_>) -> (Vec<RemoteProduct>, Option<String>) {
    let Some(cat) = resp.get_optional_child("product_catalog") else { return (vec![], None) };
    let products = cat.get_children_by_tag("product").filter_map(parse_product).collect();
    let next = cat.get_optional_child("paging").and_then(|p| text(p, "after", 512));
    (products, next)
}

pub fn parse_deleted(resp: &NodeRef<'_>) -> u32 {
    resp.get_optional_child(DELETE).and_then(|d| d.get_attr("deleted_count")).and_then(|v| v.as_str().parse::<u32>().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product() -> CatalogProduct {
        CatalogProduct {
            name: "Almarai Fresh Milk 1L".into(),
            description: Some("حليب طازج".into()),
            price_1000: Some(1250),
            currency: "BHD".into(),
            retailer_id: "100001".into(),
            image_url: Some("https://mmg.whatsapp.net/product/image/x".into()),
            hidden: false,
        }
    }

    fn child_text(n: &Node, tag: &str) -> Option<String> {
        let c = n.children()?.iter().find(|c| c.tag == tag)?;
        match c.content.as_ref()? {
            NodeContent::Bytes(b) => Some(String::from_utf8(b.clone()).unwrap()),
            NodeContent::String(s) => Some(s.to_string()),
            NodeContent::Nodes(_) => None,
        }
    }

    #[test]
    fn product_node_carries_the_exact_price_and_the_pos_picture() {
        let n = product_node(None, &product());
        assert_eq!(n.tag, "product");
        assert_eq!(n.attrs.get("is_hidden").map(|v| v.to_string()), Some("false".into()));
        assert_eq!(child_text(&n, "price").as_deref(), Some("1250"), "1.250 BHD in thousandths");
        assert_eq!(child_text(&n, "currency").as_deref(), Some("BHD"));
        assert_eq!(child_text(&n, "retailer_id").as_deref(), Some("100001"));
        assert!(child_text(&n, "id").is_none(), "a create has no id");
        let media = n.children().unwrap().iter().find(|c| c.tag == "media").unwrap();
        let img = &media.children().unwrap()[0];
        assert_eq!(child_text(img, "url").as_deref(), Some("https://mmg.whatsapp.net/product/image/x"));
        // Edit: id first; hidden flag; no price when unknown; no picture when none.
        let hidden = CatalogProduct { hidden: true, price_1000: None, image_url: None, description: None, ..product() };
        let n = product_node(Some("8431"), &hidden);
        assert_eq!(n.children().unwrap()[0].tag, "id");
        assert_eq!(n.attrs.get("is_hidden").map(|v| v.to_string()), Some("true".into()));
        assert!(child_text(&n, "price").is_none() && child_text(&n, "description").is_none());
        assert!(!n.children().unwrap().iter().any(|c| c.tag == "media"));
        let wrapped = write_content(EDIT, n);
        let NodeContent::Nodes(v) = wrapped else { panic!() };
        assert_eq!((v[0].tag.as_ref(), v[0].attrs.get("v").map(|x| x.to_string())), (EDIT, Some("1".into())));
    }

    #[test]
    fn replies_are_parsed_defensively() {
        let reply = NodeBuilder::new("iq")
            .children([NodeBuilder::new(ADD)
                .children([NodeBuilder::new("product")
                    .attr("is_hidden", "false")
                    .children([text_node("id", "7777001"), text_node("retailer_id", "100001"), text_node("name", "Milk")])
                    .build()])
                .build()])
            .build();
        let p = parse_write_reply(&reply.as_node_ref(), ADD).unwrap();
        assert_eq!((p.id.as_str(), p.retailer_id.as_deref(), p.hidden), ("7777001", Some("100001"), false));
        // An id that is not a plain token is refused.
        let bad = NodeBuilder::new("iq")
            .children([NodeBuilder::new(ADD)
                .children([NodeBuilder::new("product").children([text_node("id", "../../x")]).build()])
                .build()])
            .build();
        assert!(parse_write_reply(&bad.as_node_ref(), ADD).is_none());
        assert!(parse_write_reply(&NodeBuilder::new("iq").build().as_node_ref(), ADD).is_none());
        // Catalogue page with paging; products without ids are skipped.
        let page = NodeBuilder::new("iq")
            .children([NodeBuilder::new("product_catalog")
                .children([
                    NodeBuilder::new("product").children([text_node("id", "1"), text_node("retailer_id", "A1")]).build(),
                    NodeBuilder::new("product").children([text_node("name", "no id")]).build(),
                    NodeBuilder::new("paging").children([text_node("after", "cursor-2")]).build(),
                ])
                .build()])
            .build();
        let (items, next) = parse_list(&page.as_node_ref());
        assert_eq!(items.len(), 1);
        assert_eq!(next.as_deref(), Some("cursor-2"));
        let del = NodeBuilder::new("iq").children([NodeBuilder::new(DELETE).attr("deleted_count", "2").build()]).build();
        assert_eq!(parse_deleted(&del.as_node_ref()), 2);
    }

    // -----------------------------------------------------------------
    // Golden fixtures: the exact stanzas sent, rendered canonically
    // (attributes sorted), so any change to what goes on the wire is seen.

    fn xml(n: &Node) -> String {
        let mut attrs: Vec<(String, String)> = n.attrs.0.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        attrs.sort();
        let a: String = attrs.iter().map(|(k, v)| format!(" {k}=\"{v}\"")).collect();
        let body = match &n.content {
            None => String::new(),
            Some(NodeContent::Bytes(b)) => String::from_utf8(b.clone()).unwrap(),
            Some(NodeContent::String(s)) => s.to_string(),
            Some(NodeContent::Nodes(v)) => v.iter().map(xml).collect(),
        };
        format!("<{}{a}>{body}</{}>", n.tag, n.tag)
    }

    fn content(c: NodeContent) -> String {
        match c {
            NodeContent::Nodes(v) => v.iter().map(xml).collect(),
            _ => panic!("nodes expected"),
        }
    }

    #[test]
    fn golden_add_edit_delete_and_list_stanzas() {
        assert_eq!(
            content(write_content(ADD, product_node(None, &product()))),
            "<product_catalog_add v=\"1\"><product is_hidden=\"false\"><name>Almarai Fresh Milk 1L</name>\
             <description>حليب طازج</description><retailer_id>100001</retailer_id>\
             <media><image><url>https://mmg.whatsapp.net/product/image/x</url></image></media>\
             <price>1250</price><currency>BHD</currency></product><width>100</width><height>100</height></product_catalog_add>"
        );
        let hidden = CatalogProduct { hidden: true, image_url: None, description: None, ..product() };
        assert_eq!(
            content(write_content(EDIT, product_node(Some("8431"), &hidden))),
            "<product_catalog_edit v=\"1\"><product is_hidden=\"true\"><id>8431</id><name>Almarai Fresh Milk 1L</name>\
             <retailer_id>100001</retailer_id><price>1250</price><currency>BHD</currency></product>\
             <width>100</width><height>100</height></product_catalog_edit>"
        );
        assert_eq!(
            content(delete_content(&["11".into(), "12".into()])),
            "<product_catalog_delete v=\"1\"><product><id>11</id></product><product><id>12</id></product></product_catalog_delete>"
        );
        let jid: Jid = "97330000000@s.whatsapp.net".parse().unwrap();
        assert_eq!(
            content(list_content(&jid, 50, Some("cur-2"))),
            "<product_catalog allow_shop_source=\"true\" jid=\"97330000000@s.whatsapp.net\"><limit>50</limit>\
             <width>100</width><height>100</height><after>cur-2</after></product_catalog>"
        );
        assert!(!content(list_content(&jid, 50, None)).contains("<after>"), "the first page has no cursor");
    }

    #[test]
    fn golden_prices_are_exact_integers_in_thousandths() {
        // BHD has 3 decimals: fils map 1:1 to WhatsApp's thousandths.
        for (fils, wire) in [
            (1, "1"),
            (10, "10"),
            (100, "100"),
            (999, "999"),
            (1_000, "1000"),
            (1_250, "1250"),
            (9_990, "9990"),
            (10_005, "10005"),
            (99_999, "99999"),
            (100_000, "100000"),
            (999_999_999_999, "999999999999"),
        ] {
            let price = amwapos_core::wa_catalog::to_wa_price(fils, 3).unwrap();
            let n = product_node(None, &CatalogProduct { price_1000: Some(price), ..product() });
            assert_eq!(child_text(&n, "price").as_deref(), Some(wire), "{fils} fils");
        }
        for bad in [0, -1, 1_000_000_000_000, i64::MAX] {
            assert!(amwapos_core::wa_catalog::to_wa_price(bad, 3).is_err(), "{bad} is never sent");
        }
    }

    #[test]
    fn untrusted_replies_are_bounded_and_never_trusted_blindly() {
        // String content (not bytes), hidden flag, over-long name bounded,
        // a retailer id with spaces around it trimmed.
        let long = "x".repeat(5_000);
        let page = NodeBuilder::new("iq")
            .children([NodeBuilder::new("product_catalog")
                .children([
                    NodeBuilder::new("product")
                        .attr("is_hidden", "true")
                        .children([
                            NodeBuilder::new("id").string_content("42").build(),
                            text_node("retailer_id", "  SKU-1  "),
                            text_node("name", &long),
                        ])
                        .build(),
                    NodeBuilder::new("product").attr("is_hidden", "yes").children([text_node("id", "43")]).build(),
                    NodeBuilder::new("product").children([text_node("id", &"9".repeat(65))]).build(),
                    NodeBuilder::new("product").children([text_node("id", "a b")]).build(),
                    NodeBuilder::new("product").children([text_node("id", "")]).build(),
                ])
                .build()])
            .build();
        let (items, next) = parse_list(&page.as_node_ref());
        assert_eq!(next, None, "no paging: last page");
        assert_eq!(items.len(), 2, "ids that are too long, contain spaces or are empty are skipped");
        assert_eq!((items[0].id.as_str(), items[0].retailer_id.as_deref(), items[0].hidden), ("42", Some("SKU-1"), true));
        assert_eq!(items[0].name.as_ref().unwrap().chars().count(), 500);
        assert!(!items[1].hidden, "only the exact value true hides");
        // A reply for another operation is not taken as this one's.
        let reply = NodeBuilder::new("iq")
            .children([NodeBuilder::new(EDIT).children([NodeBuilder::new("product").children([text_node("id", "1")]).build()]).build()])
            .build();
        assert!(parse_write_reply(&reply.as_node_ref(), ADD).is_none());
        // A page without the catalogue element is empty, not an error that loops.
        assert_eq!(parse_list(&NodeBuilder::new("iq").build().as_node_ref()), (vec![], None));
        // A delete count that is not a number counts as nothing deleted.
        let del = NodeBuilder::new("iq").children([NodeBuilder::new(DELETE).attr("deleted_count", "two").build()]).build();
        assert_eq!(parse_deleted(&del.as_node_ref()), 0);
        assert!(valid_remote_id("7777001") && !valid_remote_id("<x>") && !valid_remote_id(""));
    }
}
