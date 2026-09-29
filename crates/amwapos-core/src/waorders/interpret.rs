//! Deterministic reading of one WhatsApp message: intent, item mentions
//! (quantity, size, variant words), modifications, delivery mode and a
//! Bahrain address. English, Arabic, mixed and common Manglish/transliterated
//! words; Arabic-Indic digits. Nothing here touches the catalogue: product
//! words are resolved against real products in `resolve`.

#![allow(clippy::type_complexity)]

use serde::{Deserialize, Serialize};

use crate::address::AddressParts;
use crate::ocrflow::normalize_digits;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    NewOrder,
    OrderModification,
    ProductQuestion,
    PriceQuestion,
    AvailabilityQuestion,
    DeliveryAddress,
    Payment,
    Confirmation,
    Cancellation,
    SupportIssue,
    Greeting,
    Spam,
    Unknown,
}

impl Intent {
    pub fn as_str(self) -> &'static str {
        match self {
            Intent::NewOrder => "new_order",
            Intent::OrderModification => "order_modification",
            Intent::ProductQuestion => "product_question",
            Intent::PriceQuestion => "price_question",
            Intent::AvailabilityQuestion => "availability_question",
            Intent::DeliveryAddress => "delivery_address",
            Intent::Payment => "payment",
            Intent::Confirmation => "confirmation",
            Intent::Cancellation => "cancellation",
            Intent::SupportIssue => "support_issue",
            Intent::Greeting => "greeting",
            Intent::Spam => "spam",
            Intent::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Intent> {
        Some(match s {
            "new_order" => Intent::NewOrder,
            "order_modification" => Intent::OrderModification,
            "product_question" => Intent::ProductQuestion,
            "price_question" => Intent::PriceQuestion,
            "availability_question" => Intent::AvailabilityQuestion,
            "delivery_address" => Intent::DeliveryAddress,
            "payment" => Intent::Payment,
            "confirmation" => Intent::Confirmation,
            "cancellation" => Intent::Cancellation,
            "support_issue" => Intent::SupportIssue,
            "greeting" => Intent::Greeting,
            "spam" => Intent::Spam,
            "unknown" => Intent::Unknown,
            _ => return None,
        })
    }

    /// Intents that can start or change an order draft.
    pub fn is_ordering(self) -> bool {
        matches!(
            self,
            Intent::NewOrder
                | Intent::OrderModification
                | Intent::DeliveryAddress
                | Intent::Confirmation
                | Intent::Cancellation
                | Intent::Payment
        )
    }
}

/// Quantity as the customer wrote it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Qty {
    /// milli-units (1 → 1000, "half" → 500).
    pub milli: i64,
    /// pcs | kg | g | carton | pack | dozen
    pub unit: String,
    /// Written by the customer (not a default of 1).
    pub explicit: bool,
}

/// One product mention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mention {
    pub text: String,
    pub qty: Qty,
    /// Normalized product words (lexicon-expanded), size/quantity removed.
    pub words: Vec<String>,
    /// Explicit size: (value, "ml" | "g").
    pub size: Option<(i64, String)>,
    /// small | big | medium
    pub size_word: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Modification {
    /// "make coke 3", "coke 3 instead"
    SetQty { target: Mention },
    /// "remove milk", "no milk"
    Remove { target: Mention },
    /// "add 2 water", "also lays"
    Add { item: Mention },
    /// "same but coke zero", "change milk to low fat milk"
    Replace { from: Option<Mention>, to: Mention },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddressMention {
    pub parts: AddressParts,
    pub area: Option<String>,
    pub raw: String,
    /// "same address", "home": the saved address is meant.
    pub saved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    pub intent: Intent,
    /// high | medium | low
    pub band: String,
    pub reasons: Vec<String>,
    pub items: Vec<Mention>,
    pub modifications: Vec<Modification>,
    pub address: Option<AddressMention>,
    /// delivery | pickup
    pub mode: Option<String>,
    pub priority: Vec<String>,
    pub arabic: bool,
}

// ---------------------------------------------------------------- normalization

/// Arabic letter variants folded (أإآ→ا, ة→ه, ى→ي), tatweel and diacritics
/// removed, Arabic digits → ASCII, lower case.
pub fn normalize(s: &str) -> String {
    normalize_digits(s)
        .chars()
        .filter(|c| !('\u{064B}'..='\u{0652}').contains(c) && *c != '\u{0640}')
        .map(|c| match c {
            'أ' | 'إ' | 'آ' => 'ا',
            'ة' => 'ه',
            'ى' => 'ي',
            '،' => ',',
            '؟' => '?',
            _ => c,
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn has_arabic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c))
}

/// Everyday product words in Arabic / Manglish → an English catalogue word.
/// Used to find candidates; the catalogue's own `name_ar` is also searched.
pub const LEXICON: &[(&str, &str)] = &[
    ("حليب", "milk"),
    ("لبن", "laban"),
    ("ماي", "water"),
    ("ماء", "water"),
    ("مويه", "water"),
    ("مياه", "water"),
    ("خبز", "bread"),
    ("عيش", "rice"),
    ("رز", "rice"),
    ("ارز", "rice"),
    ("سكر", "sugar"),
    ("شاي", "tea"),
    ("قهوه", "coffee"),
    ("بيض", "eggs"),
    ("دجاج", "chicken"),
    ("جبن", "cheese"),
    ("جبنه", "cheese"),
    ("زبده", "butter"),
    ("عصير", "juice"),
    ("بيبسي", "pepsi"),
    ("كوكا", "coca cola"),
    ("كوكاكولا", "coca cola"),
    ("كولا", "cola"),
    ("شيبس", "chips"),
    ("جيبس", "chips"),
    ("ليز", "lays"),
    ("مناديل", "tissue"),
    ("صابون", "soap"),
    ("زيت", "oil"),
    ("طحين", "flour"),
    ("ملح", "salt"),
    ("طماطم", "tomato"),
    ("بصل", "onion"),
    ("موز", "banana"),
    ("تفاح", "apple"),
    ("زبادي", "yoghurt"),
    ("دايت", "diet"),
    ("زيرو", "zero"),
    ("كامل", "full"),
    ("قليل", "low"),
    ("الدسم", "fat"),
    ("دسم", "fat"),
    ("paal", "milk"),
    ("pal", "milk"),
    ("vellam", "water"),
    ("ari", "rice"),
    ("panchasara", "sugar"),
    ("chaya", "tea"),
    ("mutta", "eggs"),
    ("kozhi", "chicken"),
    ("coke", "coca cola"),
    ("cocacola", "coca cola"),
    ("lays", "lays"),
    ("lay's", "lays"),
    ("chips", "chips"),
    ("crisps", "chips"),
    ("h2o", "water"),
    ("fullfat", "full fat"),
    ("lowfat", "low fat"),
    ("yogurt", "yoghurt"),
];

const STOP: &[&str] = &[
    "please",
    "pls",
    "plz",
    "and",
    "also",
    "want",
    "need",
    "send",
    "give",
    "me",
    "i",
    "a",
    "an",
    "the",
    "of",
    "to",
    "for",
    "some",
    "bring",
    "get",
    "order",
    "my",
    "with",
    "can",
    "you",
    "u",
    "we",
    "us",
    "want",
    "would",
    "like",
    "have",
    "just",
    "ok",
    "okay",
    "bro",
    "habibi",
    "dear",
    "sir",
    "madam",
    "thanks",
    "thank",
    "thx",
    "hi",
    "hello",
    "salam",
    "one",
    "more",
    "pcs",
    "pc",
    "piece",
    "pieces",
    "bottle",
    "bottles",
    "nos",
    "number",
    "items",
    "item",
    "x",
    "و",
    "ابي",
    "ابغي",
    "ابغا",
    "ابغى",
    "اريد",
    "بغيت",
    "لو",
    "سمحت",
    "من",
    "فضلك",
    "عطني",
    "ارسل",
    "ارسلي",
    "حبه",
    "حبات",
    "حبة",
    "مع",
    "عندي",
    "abc",
    "venam",
    "venom",
    "kodukku",
    "tharumo",
    "etta",
    "chetta",
    "bhai",
    "bhaiya",
    "yes",
    "no",
    "yeah",
    "yep",
    "sure",
    "fine",
    "good",
    "nice",
];

/// Words that never name a product (questions, time, waiting, courtesy).
const NOT_PRODUCT: &[&str] = &[
    "where",
    "what",
    "when",
    "which",
    "who",
    "why",
    "how",
    "is",
    "are",
    "was",
    "were",
    "am",
    "be",
    "it",
    "this",
    "that",
    "there",
    "here",
    "my",
    "your",
    "order",
    "orders",
    "waiting",
    "wait",
    "still",
    "urgent",
    "asap",
    "now",
    "today",
    "tomorrow",
    "tonight",
    "later",
    "time",
    "open",
    "close",
    "closed",
    "shop",
    "store",
    "delivery",
    "deliver",
    "delivered",
    "received",
    "late",
    "long",
    "minutes",
    "hour",
    "hours",
    "status",
    "update",
    "help",
    "problem",
    "issue",
    "not",
    "yet",
    "again",
    "same",
    "vegam",
    "evide",
    "enthu",
    "وين",
    "متي",
    "متى",
    "الطلب",
    "طلبي",
    "الحين",
    "اليوم",
    "بكره",
    "مفتوح",
    "مسكر",
    "please",
    "hurry",
    "quickly",
    "fast",
    "ok",
    "ready",
    "done",
];

const SIZE_WORDS: &[(&str, &str)] = &[
    ("small", "small"),
    ("smal", "small"),
    ("mini", "small"),
    ("little", "small"),
    ("sm", "small"),
    ("صغير", "small"),
    ("صغيره", "small"),
    ("cheriya", "small"),
    ("cheriyath", "small"),
    ("big", "big"),
    ("large", "big"),
    ("larg", "big"),
    ("jumbo", "big"),
    ("family", "big"),
    ("xl", "big"),
    ("كبير", "big"),
    ("كبيره", "big"),
    ("valiya", "big"),
    ("valuth", "big"),
    ("medium", "medium"),
    ("med", "medium"),
    ("وسط", "medium"),
];

fn number_word(w: &str) -> Option<i64> {
    Some(match w {
        "one" | "a" | "an" | "single" | "واحد" | "وحده" | "وحدة" | "onnu" | "oru" => 1000,
        "two" | "couple" | "اثنين" | "ثنتين" | "اثنان" | "randu" => 2000,
        "three" | "ثلاث" | "ثلاثه" | "moonu" | "munnu" => 3000,
        "four" | "اربع" | "اربعه" | "naalu" | "nalu" => 4000,
        "five" | "خمس" | "خمسه" | "anju" => 5000,
        "six" | "ست" | "سته" | "aaru" => 6000,
        "seven" | "سبع" | "سبعه" | "ezhu" => 7000,
        "eight" | "ثمان" | "ثمانيه" | "ettu" => 8000,
        "nine" | "تسع" | "تسعه" | "onpathu" => 9000,
        "ten" | "عشر" | "عشره" | "pathu" => 10_000,
        "dozen" | "درزن" => 12_000,
        "half" | "نص" | "نصف" | "ara" => 500,
        _ => return None,
    })
}

/// "2", "2x", "x2", "٢" → milli.
fn number_token(w: &str) -> Option<i64> {
    let t = w.trim_start_matches(['x', '×']).trim_end_matches(['x', '×']);
    if t.is_empty() || t.len() > 5 {
        return None;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return t.parse::<i64>().ok().filter(|n| (1..=999).contains(n)).map(|n| n * 1000);
    }
    // "1/2"
    if t == "1/2" {
        return Some(500);
    }
    None
}

fn unit_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "kg" | "kilo" | "kilos" | "كيلو" | "kilogram" => "kg",
        "g" | "gm" | "gram" | "grams" | "gms" | "جرام" | "غرام" => "g",
        "carton" | "cartons" | "ctn" | "box" | "boxes" | "كرتون" | "كرتونه" | "كراتين" => "carton",
        "pack" | "packs" | "pkt" | "packet" | "packets" | "باكيت" | "علبه" | "بكت" => "pack",
        "dozen" | "درزن" => "dozen",
        _ => return None,
    })
}

/// A size token: "330ml", "1.5l", "2.25", "500g", "1 liter".
pub fn size_token(w: &str, next: Option<&str>) -> Option<(i64, String)> {
    let w = w.replace(',', ".");
    let end = w.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(w.len());
    let (n, u) = w.split_at(end);
    if n.is_empty() || n == "." {
        return None;
    }
    let v: f64 = n.parse().ok()?;
    let u = if u.is_empty() { next.unwrap_or("") } else { u };
    let (mul, unit) = match u.trim_matches('.') {
        "ml" | "مل" => (1.0, "ml"),
        "l" | "ltr" | "lt" | "litre" | "liter" | "liters" | "litres" | "لتر" => (1000.0, "ml"),
        "cl" => (10.0, "ml"),
        "g" | "gm" | "gms" | "gram" | "grams" | "جرام" | "غرام" => (1.0, "g"),
        "kg" | "kgs" | "kilo" | "كيلو" => (1000.0, "g"),
        _ => return None,
    };
    Some(((v * mul).round() as i64, unit.to_string()))
}

fn any(t: &str, words: &[&str]) -> bool {
    words.iter().any(|w| t.contains(w))
}

fn word_in(t: &str, words: &[&str]) -> bool {
    t.split(|c: char| !c.is_alphanumeric() && c != '\'').any(|w| words.contains(&w))
}

// ---------------------------------------------------------------- address

/// Bahrain address parts written in a message.
pub fn parse_address(text: &str) -> Option<AddressMention> {
    let t = normalize(text);
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ',' || c == ':' || c == '-').filter(|w| !w.is_empty()).collect();
    let mut p = AddressParts::default();
    let mut found = false;
    let num_after = |i: usize| -> Option<String> {
        words
            .get(i + 1)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|w| w.chars().any(|c| c.is_ascii_digit()) && w.len() <= 6)
            .map(|w| w.to_string())
    };
    for (i, w) in words.iter().enumerate() {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        // "blk221", "rd1234"
        let (head, tail) = w.split_at(w.find(|c: char| c.is_ascii_digit()).unwrap_or(w.len()));
        let inline = (!tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())).then(|| tail.to_string());
        let key = if inline.is_some() { head } else { w };
        let val = inline.clone().or_else(|| num_after(i));
        match key {
            "block" | "blk" | "blok" | "مجمع" | "بلوك" => {
                if let Some(v) = val.filter(|v| v.chars().all(|c| c.is_ascii_digit())) {
                    p.block = Some(v);
                    found = true;
                }
            }
            "road" | "rd" | "street" | "st" | "طريق" | "شارع" => {
                if let Some(v) = val {
                    p.road = Some(v);
                    found = true;
                }
            }
            "building" | "bldg" | "bld" | "house" | "villa" | "h" | "مبني" | "مبنى" | "منزل" | "بيت" | "فيلا" => {
                if let Some(v) = val {
                    p.building = Some(v);
                    found = true;
                }
            }
            "flat" | "apt" | "apartment" | "شقه" | "شقة" => {
                if let Some(v) = val {
                    p.flat = Some(v);
                    found = true;
                }
            }
            _ => {}
        }
    }
    let saved =
        word_in(&t, &["home", "same"]) && any(&t, &["same address", "my home", "to home", "home", "نفس العنوان", "البيت"]) && !found;
    // Landmark: "near X", "opposite X", "قرب", "مقابل".
    for key in ["near ", "opposite ", "next to ", "behind ", "قرب ", "مقابل ", "جنب ", "عند "] {
        if let Some(pos) = t.find(key) {
            let rest: String = t[pos..].chars().take(60).collect();
            p.landmark = Some(rest.split([',', '.', '\n']).next().unwrap_or("").trim().to_string()).filter(|x| !x.is_empty());
            found = true;
            break;
        }
    }
    if !found && !saved {
        return None;
    }
    Some(AddressMention { parts: p, area: None, raw: text.chars().take(300).collect(), saved })
}

// ---------------------------------------------------------------- segments

fn split_items(text: &str) -> Vec<String> {
    let t = normalize(text);
    let mut parts: Vec<String> = vec![];
    for line in t.split(['\n', ',', ';', '+', '&']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // " and " / " و" (a separate word or a prefix of the next word).
        let mut cur: Vec<String> = vec![];
        for w in line.split_whitespace() {
            let is_and = matches!(w, "and" | "n" | "also" | "plus" | "و");
            let prefixed = w.starts_with('و') && w.chars().count() > 3 && !matches!(w, "وسط" | "وحده" | "وحدة" | "واحد");
            if is_and || prefixed {
                if !cur.is_empty() {
                    parts.push(cur.join(" "));
                    cur.clear();
                }
                if prefixed {
                    cur.push(w.chars().skip(1).collect());
                }
                continue;
            }
            // "2 coke 3 milk": a quantity followed by product words, after
            // other product words, starts a new item ("coke x3", "milk 2" do not).
            let next_word = line.split_whitespace().skip_while(|x| *x != w).nth(1);
            let next_is_word = next_word.is_some_and(|n| {
                n.chars().next().is_some_and(|c| c.is_alphabetic()) && unit_word(n).is_none() && size_token(n, None).is_none()
            }) && size_token(w, next_word).is_none();
            if (number_token(w).is_some() || number_word(w).is_some() && w != "a")
                && !w.starts_with(['x', '×'])
                && next_is_word
                && cur.iter().any(|x| number_token(x).is_none() && number_word(x).is_none() && x.chars().any(|c| c.is_alphabetic()))
            {
                let last_is_unit = cur.last().is_some_and(|x| unit_word(x).is_some());
                if !last_is_unit {
                    parts.push(cur.join(" "));
                    cur.clear();
                }
            }
            cur.push(w.to_string());
        }
        if !cur.is_empty() {
            parts.push(cur.join(" "));
        }
    }
    parts
}

/// Read one item segment ("2 coke big", "lays cheese", "milk small", "half kilo tomato").
pub fn parse_item(seg: &str) -> Option<Mention> {
    let t = normalize(seg);
    let words: Vec<&str> = t
        .split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let mut qty: Option<(i64, String)> = None;
    let mut size = None;
    let mut size_word = None;
    let mut out = vec![];
    let mut skip = 0;
    for (i, w) in words.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let next = words.get(i + 1).copied();
        // "500g", "1.5 l", "330ml"
        if let Some(sz) = size_token(w, next) {
            let unit_in_word = w.find(|c: char| c.is_alphabetic()).is_some();
            // "half kilo" / "1 kg" of a loose product: that is a quantity by weight.
            if sz.1 == "g"
                && qty.is_none()
                && (w.ends_with("kg") || next == Some("kg") || next == Some("kilo") || next == Some("كيلو"))
                && out.is_empty()
            {
                qty = Some((sz.0, "g".into()));
            } else {
                size = Some(sz);
            }
            if !unit_in_word {
                skip = 1;
            }
            continue;
        }
        if let Some(n) = number_token(w).or_else(|| number_word(w)) {
            // "2.25" alone is not a quantity (sizes have decimals).
            if w.contains('.') {
                continue;
            }
            if qty.is_none() {
                // "half kilo": weight.
                if let Some(u) = next.and_then(unit_word) {
                    let (m, unit) = match u {
                        "kg" => (n, "g"),
                        "g" => (n / 1000, "g"),
                        other => (n, other),
                    };
                    let m = if u == "kg" { m * 1000 / 1000 } else { m };
                    qty = Some((m, unit.to_string()));
                    skip = 1;
                    continue;
                }
                qty = Some((n, "pcs".into()));
                continue;
            }
        }
        if let Some(u) = unit_word(w) {
            if qty.is_none() {
                qty = Some((1000, u.to_string()));
            } else if let Some(q) = qty.as_mut() {
                if q.1 == "pcs" {
                    q.1 = u.into();
                }
            }
            continue;
        }
        if let Some((_, s)) = SIZE_WORDS.iter().find(|(k, _)| k == w) {
            size_word = Some(s.to_string());
            continue;
        }
        if STOP.contains(w) || NOT_PRODUCT.contains(w) {
            continue;
        }
        let w = w.trim_start_matches("ال").to_string();
        let w = if w.is_empty() { words[i].to_string() } else { w };
        match LEXICON.iter().find(|(k, _)| *k == w || *k == words[i]) {
            Some((_, e)) => out.extend(e.split_whitespace().map(|x| x.to_string())),
            None => {
                if w.chars().any(|c| c.is_alphabetic()) && w.chars().count() >= 2 {
                    out.push(w);
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    let (milli, unit, explicit) = match qty {
        Some((m, u)) if u == "g" => (m, "kg".to_string(), true),
        Some((m, u)) if u == "dozen" => (m * 12, "pcs".to_string(), true),
        Some((m, u)) => (m, u, true),
        None => (1000, "pcs".to_string(), false),
    };
    out.dedup();
    Some(Mention { text: seg.trim().chars().take(120).collect(), qty: Qty { milli, unit, explicit }, words: out, size, size_word })
}

// ---------------------------------------------------------------- intent

const GREETINGS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "salam",
    "salaam",
    "assalamualaikum",
    "good morning",
    "good evening",
    "good afternoon",
    "السلام عليكم",
    "مرحبا",
    "هلا",
    "صباح الخير",
    "مساء الخير",
    "thanks",
    "thank you",
    "thx",
    "شكرا",
    "ok thanks",
    "namaskaram",
];
const CANCEL: &[&str] =
    &["cancel", "cancel it", "cancel order", "no need", "dont need", "don't need", "الغي", "الغاء", "كنسل", "لا ابغي", "venda", "vendaa"];
const CONFIRM: &[&str] = &[
    "yes",
    "yes please",
    "ok",
    "okay",
    "confirm",
    "confirmed",
    "done",
    "go ahead",
    "correct",
    "تمام",
    "نعم",
    "ايوه",
    "اوكي",
    "اكيد",
    "sari",
    "ok ok",
    "👍",
];
const PAID: &[&str] =
    &["paid", "transferred", "payment sent", "sent the money", "benefit", "benefitpay", "حولت", "دفعت", "تم التحويل", "حوالة"];
const URGENT: &[&str] = &["urgent", "asap", "quickly", "fast please", "hurry", "emergency", "بسرعه", "ضروري", "عاجل", "vegam"];
const WAITING: &[&str] =
    &["still waiting", "where is my order", "not received", "not delivered", "how long", "late", "ما وصل", "وين الطلب", "تاخر", "متاخر"];
const COMPLAINT: &[&str] =
    &["wrong", "broken", "damaged", "expired", "missing", "complain", "refund", "bad", "خطا", "خربان", "منتهي", "ناقص", "شكوي", "استرجاع"];

/// Read one message. `active` = there is an open draft for this chat;
/// `kind` = text | image | document | other.
pub fn read(kind: &str, text: &str, active: bool) -> Reading {
    let raw = text.trim();
    let t = normalize(raw);
    let arabic = has_arabic(raw);
    let mut reasons = vec![];
    let mut priority = vec![];
    let mut out = Reading {
        intent: Intent::Unknown,
        band: "low".into(),
        reasons: vec![],
        items: vec![],
        modifications: vec![],
        address: None,
        mode: None,
        priority: vec![],
        arabic,
    };
    if any(&t, URGENT) {
        priority.push("urgent_request".into());
    }
    if any(&t, WAITING) {
        priority.push("customer_waiting".into());
    }
    if any(&t, COMPLAINT) {
        priority.push("complaint".into());
    }
    // Spam: a link with promotion words, or a known pattern.
    if any(&t, &["http://", "https://", "www."])
        && any(&t, &["win", "prize", "crypto", "bitcoin", "free gift", "click", "investment", "loan", "لقد ربحت", "جائزه", "اربح"])
    {
        out.intent = Intent::Spam;
        out.band = "high".into();
        out.reasons = vec!["link_with_promotion_words".into()];
        return out;
    }
    if kind == "image" || kind == "document" {
        out.intent = if active || any(&t, PAID) || t.is_empty() { Intent::Payment } else { Intent::Unknown };
        out.band = if active { "medium".into() } else { "low".into() };
        out.reasons = vec![format!("{kind}_attachment")];
        out.priority = priority;
        return out;
    }
    if kind != "text" || t.is_empty() {
        out.reasons = vec!["unsupported_message".into()];
        return out;
    }
    let bare = t.trim_matches(|c: char| !c.is_alphanumeric() && !has_arabic(&c.to_string())).trim();
    if GREETINGS.contains(&bare) {
        out.intent = Intent::Greeting;
        out.band = "high".into();
        out.reasons = vec!["greeting_only".into()];
        return out;
    }
    if CANCEL.iter().any(|c| bare == *c || bare.starts_with(&format!("{c} ")) || bare.ends_with(&format!(" {c}")))
        || (active && word_in(&t, &["cancel", "الغي", "الغاء"]))
    {
        out.intent = Intent::Cancellation;
        out.band = "high".into();
        out.reasons = vec!["cancel_words".into()];
        out.priority = priority;
        return out;
    }
    if active && CONFIRM.contains(&bare) {
        out.intent = Intent::Confirmation;
        out.band = "high".into();
        out.reasons = vec!["confirmation_words".into()];
        return out;
    }
    if any(&t, PAID) {
        out.intent = Intent::Payment;
        out.band = "medium".into();
        out.reasons = vec!["payment_words".into()];
        out.priority = priority;
        return out;
    }
    // Delivery / pickup and address.
    if word_in(&t, &["pickup", "collect", "takeaway", "استلام", "باخذه", "باخذها"])
        || any(&t, &["pick up", "i will come", "ill come", "will collect", "بجي اخذ"])
    {
        out.mode = Some("pickup".into());
    } else if any(&t, &["deliver", "delivery", "send to", "توصيل", "وصل", "ارسل ل"]) {
        out.mode = Some("delivery".into());
    }
    let address = parse_address(raw);
    if address.is_some() {
        out.mode.get_or_insert("delivery".into());
    }
    // Remove the address part before reading items.
    let item_text = strip_address(&t);
    let question = t.contains('?')
        || any(&t, &["do you have", "is there", "have you", "available", "in stock", "عندكم", "فيه", "متوفر", "undo", "und"]);
    let price_q = any(&t, &["how much", "price", "cost", "rate", "كم سعر", "بكم", "كم", "سعر", "ethra", "evide price"]);
    // Modifications (only meaningful with an open draft).
    if active {
        if let Some(m) = modification(&item_text) {
            out.modifications.push(m);
            out.intent = Intent::OrderModification;
            out.band = "medium".into();
            reasons.push("modification_words".into());
        }
    }
    if out.modifications.is_empty() {
        for seg in split_items(&item_text) {
            if let Some(m) = parse_item(&seg) {
                // Leftover delivery words are not products.
                if m.words.iter().all(|w| {
                    matches!(w.as_str(), "deliver" | "delivery" | "to" | "at" | "توصيل" | "pickup" | "later" | "now" | "today" | "tomorrow")
                }) {
                    continue;
                }
                out.items.push(m);
            }
        }
    }
    if !out.modifications.is_empty() {
        // done above
    } else if !priority.is_empty() && priority.iter().any(|p| p != "urgent_request") && !out.items.iter().any(|i| i.qty.explicit) {
        out.intent = Intent::SupportIssue;
        out.band = "medium".into();
        reasons.push("support_words".into());
        out.items.clear();
    } else if price_q && !out.items.is_empty() {
        out.intent = Intent::PriceQuestion;
        out.band = "medium".into();
        reasons.push("price_words".into());
    } else if question && !out.items.is_empty() {
        out.intent = Intent::AvailabilityQuestion;
        out.band = "medium".into();
        reasons.push("question_words".into());
    } else if !out.items.is_empty() {
        out.intent = if active { Intent::OrderModification } else { Intent::NewOrder };
        out.band = if out.items.iter().any(|i| i.qty.explicit) || out.mode.is_some() { "high".into() } else { "medium".into() };
        reasons.push("item_mentions".into());
        if active {
            // Items in an open draft are additions.
            out.modifications = out.items.drain(..).map(|item| Modification::Add { item }).collect();
        }
    } else if address.is_some() || out.mode.is_some() {
        out.intent = Intent::DeliveryAddress;
        out.band = "high".into();
        reasons.push("address_words".into());
    } else if !priority.is_empty() {
        out.intent = Intent::SupportIssue;
        out.band = "medium".into();
        reasons.push("support_words".into());
    } else if question || price_q {
        out.intent = Intent::ProductQuestion;
        out.band = "low".into();
        reasons.push("question_without_items".into());
    } else {
        reasons.push("no_rule_matched".into());
    }
    out.address = address;
    out.reasons = reasons;
    out.priority = priority;
    out
}

fn strip_address(t: &str) -> String {
    let mut out = String::new();
    for part in t.split(['\n', ',']) {
        let p = part.trim();
        if parse_address(p).is_some() {
            // keep the product words before the address words ("milk deliver to block 221")
            let cut = [
                "deliver",
                "delivery",
                "send to",
                "block",
                "blk",
                "road",
                "building",
                "bldg",
                "flat",
                "house",
                "توصيل",
                "مجمع",
                "طريق",
                "مبني",
                "شقه",
                "near",
                "قرب",
            ]
            .iter()
            .filter_map(|k| p.find(k))
            .min()
            .unwrap_or(p.len());
            out.push_str(&p[..cut]);
        } else {
            let cut = ["deliver to", "delivery to", "send to", "توصيل"].iter().filter_map(|k| p.find(k)).min().unwrap_or(p.len());
            out.push_str(&p[..cut]);
        }
        out.push('\n');
    }
    out
}

fn modification(t: &str) -> Option<Modification> {
    let t = t.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    let first = *words.first()?;
    let rest = |n: usize| words[n.min(words.len())..].join(" ");
    // remove / no / without / delete
    if matches!(first, "remove" | "delete" | "no" | "without" | "drop" | "شيل" | "بدون" | "الغ" | "لا")
        || t.starts_with("take out")
        || t.starts_with("don't want")
        || t.starts_with("dont want")
    {
        let skip = if t.starts_with("take out") || t.starts_with("don't want") || t.starts_with("dont want") { 2 } else { 1 };
        return parse_item(&rest(skip)).map(|target| Modification::Remove { target });
    }
    // add / also / plus / one more
    if matches!(first, "add" | "also" | "plus" | "زيد" | "ضيف" | "كمان") || t.starts_with("one more") || t.starts_with("and ") {
        let skip = if t.starts_with("one more") { 0 } else { 1 };
        return parse_item(&rest(skip)).map(|item| Modification::Add { item });
    }
    // "same but coke zero", "instead coke zero", "change milk to low fat milk"
    if t.starts_with("same but") || first == "instead" || t.contains(" instead") {
        let tail = t.trim_start_matches("same but").trim_start_matches("instead").replace(" instead", "");
        return parse_item(&tail).map(|to| Modification::Replace { from: None, to });
    }
    if matches!(first, "change" | "replace" | "switch" | "غير" | "بدل") {
        let body = rest(1);
        if let Some((a, b)) = body
            .split_once(" to ")
            .or_else(|| body.split_once(" with "))
            .or_else(|| body.split_once(" الى "))
            .or_else(|| body.split_once(" ب"))
        {
            let from = parse_item(a);
            // "change coke to 3": a quantity.
            if let (Some(f), Some(n)) = (from.clone(), number_token(b.trim()).or_else(|| number_word(b.trim()))) {
                let mut target = f;
                target.qty = Qty { milli: n, unit: "pcs".into(), explicit: true };
                return Some(Modification::SetQty { target });
            }
            return parse_item(b).map(|to| Modification::Replace { from, to });
        }
    }
    // "make coke 3", "coke 3 only", "make it 3 coke"
    if matches!(first, "make" | "set" | "خلي" | "خليه" | "خليها") {
        return parse_item(&rest(1)).filter(|m| m.qty.explicit).map(|target| Modification::SetQty { target });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(text: &str) -> Vec<(i64, Vec<String>, Option<(i64, String)>, Option<String>)> {
        read("text", text, false).items.into_iter().map(|m| (m.qty.milli, m.words, m.size, m.size_word)).collect()
    }

    #[test]
    fn the_example_order() {
        let r = read("text", "2 coke big, one lays cheese, milk small and deliver to block 221", false);
        assert_eq!(r.intent, Intent::NewOrder);
        assert_eq!(r.mode.as_deref(), Some("delivery"));
        assert_eq!(r.address.as_ref().unwrap().parts.block.as_deref(), Some("221"));
        let it = r.items;
        assert_eq!(it.len(), 3, "{it:?}");
        assert_eq!(
            (it[0].qty.milli, it[0].words.clone(), it[0].size_word.as_deref()),
            (2000, vec!["coca".to_string(), "cola".to_string()], Some("big"))
        );
        assert_eq!((it[1].qty.milli, it[1].words.clone()), (1000, vec!["lays".to_string(), "cheese".to_string()]));
        assert_eq!((it[2].qty.explicit, it[2].words.clone(), it[2].size_word.as_deref()), (false, vec!["milk".to_string()], Some("small")));
    }

    #[test]
    fn quantities() {
        assert_eq!(items("two pepsi")[0].0, 2000);
        assert_eq!(items("٢ بيبسي")[0].0, 2000);
        assert_eq!(items("half kilo tomato")[0].0, 500);
        assert_eq!(items("500g cheese")[0].2, Some((500, "g".into())), "a size, not a weight quantity when not loose");
        assert_eq!(items("1 carton water")[0].0, 1000);
        assert_eq!(read("text", "1 carton water", false).items[0].qty.unit, "carton");
        assert_eq!(items("a dozen eggs")[0].0, 12_000);
        assert_eq!(
            items("2 x 1.5L coke")[0],
            (2000, vec!["coca".into(), "cola".into()], Some((1500, "ml".into())), None),
            "2 bottles of 1.5 L, not 3 litres"
        );
        assert_eq!(items("coke x3")[0].0, 3000);
        assert_eq!(items("randu paal")[0], (2000, vec!["milk".into()], None, None));
    }

    #[test]
    fn arabic_and_mixed() {
        let r = read("text", "ابغى ٢ حليب و ٣ خبز وتوصيل مجمع 221", false);
        assert_eq!(r.intent, Intent::NewOrder, "{r:?}");
        assert!(r.arabic);
        let w: Vec<Vec<String>> = r.items.iter().map(|m| m.words.clone()).collect();
        assert_eq!(w, vec![vec!["milk".to_string()], vec!["bread".to_string()]], "{:?}", r.items);
        assert_eq!(r.items[1].qty.milli, 3000);
        assert_eq!(r.address.unwrap().parts.block.as_deref(), Some("221"));
        let r = read("text", "2 pepsi zero و chips كبير", false);
        assert_eq!(r.items.len(), 2, "{r:?}");
        assert_eq!(r.items[1].size_word.as_deref(), Some("big"));
    }

    #[test]
    fn questions_are_not_orders() {
        assert_eq!(read("text", "how much is coke 1.5L?", false).intent, Intent::PriceQuestion);
        assert_eq!(read("text", "do you have lays cheese?", false).intent, Intent::AvailabilityQuestion);
        assert_eq!(read("text", "are you open now?", false).intent, Intent::ProductQuestion);
        assert_eq!(read("text", "Hello", false).intent, Intent::Greeting);
        assert_eq!(read("text", "السلام عليكم", false).intent, Intent::Greeting);
        assert_eq!(read("text", "WIN a free gift!! click https://x.example/promo", false).intent, Intent::Spam);
    }

    #[test]
    fn modifications_in_an_open_draft() {
        let m = |t: &str| read("text", t, true);
        assert!(matches!(&m("make coke 3").modifications[0], Modification::SetQty { target } if target.qty.milli == 3000));
        assert!(matches!(&m("remove milk").modifications[0], Modification::Remove { target } if target.words == vec!["milk".to_string()]));
        assert!(matches!(&m("add 2 water").modifications[0], Modification::Add { item } if item.qty.milli == 2000));
        assert!(
            matches!(&m("same but coke zero").modifications[0], Modification::Replace { to, .. } if to.words.contains(&"zero".to_string()))
        );
        assert!(matches!(&m("change coke to 4").modifications[0], Modification::SetQty { target } if target.qty.milli == 4000));
        assert_eq!(m("cancel it").intent, Intent::Cancellation);
        assert_eq!(m("yes").intent, Intent::Confirmation);
        assert_eq!(read("text", "yes", false).intent, Intent::Unknown, "\"yes\" with no open draft is not an order");
        assert_eq!(m("2 lays").intent, Intent::OrderModification);
    }

    #[test]
    fn address_and_priority() {
        let a = parse_address("Flat 12, Bldg 1203, Road 4518, Block 245 near the mosque").unwrap();
        assert_eq!(
            (a.parts.flat.as_deref(), a.parts.building.as_deref(), a.parts.road.as_deref(), a.parts.block.as_deref()),
            (Some("12"), Some("1203"), Some("4518"), Some("245"))
        );
        assert_eq!(a.parts.landmark.as_deref(), Some("near the mosque"));
        assert_eq!(parse_address("شقة 3 مبنى 55 طريق 12 مجمع 338").unwrap().parts.block.as_deref(), Some("338"));
        assert!(parse_address("send 2 milk").is_none());
        let r = read("text", "where is my order?? still waiting, urgent", false);
        assert!(r.priority.contains(&"urgent_request".to_string()) && r.priority.contains(&"customer_waiting".to_string()));
        assert_eq!(r.intent, Intent::SupportIssue);
        assert_eq!(read("image", "", true).intent, Intent::Payment);
        assert_eq!(read("text", "I paid by benefit", true).intent, Intent::Payment);
        assert_eq!(read("text", "I will pick up at 6", true).mode.as_deref(), Some("pickup"));
    }
}
