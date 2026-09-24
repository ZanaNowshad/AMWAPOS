#![allow(dead_code)]

use std::sync::Arc;

use amwapos_core::catalog::{ProductCreate, ProductInput};
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_core::setup::SetupRequest;

pub struct Env {
    pub dir: tempfile::TempDir,
    pub core: AppCore,
    pub owner_id: String,
    pub owner_token: String,
}

pub const OWNER_PIN: &str = "4826";

pub fn op() -> String {
    ulid::Ulid::new().to_string()
}

pub fn setup_request() -> SetupRequest {
    serde_json::from_value(serde_json::json!({
        "business_name": "Al Noor Supermarket",
        "cr_number": "12345-1",
        "vat_number": "200000000000003",
        "branch_name": "Manama",
        "vat_rate_bp": 1000,
        "prices_include_vat": true,
        "owner_name": "Owner",
        "owner_pin": OWNER_PIN,
        "device_name": "Till 1",
        "device_code": "T01"
    }))
    .unwrap()
}

pub fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let core = AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap();
    core.setup_initialize(setup_request()).unwrap();
    let users = core.login_users().unwrap();
    let owner_id = users[0].user_id.clone();
    let login = core.login(&owner_id, OWNER_PIN).unwrap();
    Env { dir, core, owner_id, owner_token: login.token }
}

impl Env {
    pub fn tax_rule(&self) -> String {
        self.core.tax_rules_list(&self.owner_token).unwrap().into_iter().find(|t| t.rate_bp == 1000).unwrap().tax_rule_id
    }

    pub fn product(&self, name: &str, barcode: &str, price: i64, cost: i64, stock_milli: i64) -> String {
        let req = ProductCreate {
            product: ProductInput {
                sku: None,
                name: name.into(),
                name_ar: None,
                description: None,
                category_id: None,
                tax_rule_id: self.tax_rule(),
                unit: "pcs".into(),
                track_inventory: true,
                allow_decimal_quantity: false,
                reorder_point_milli: 2000,
                is_favorite: false,
            },
            price_minor: price,
            cost_minor: Some(cost),
            barcodes: vec![barcode.into()],
            opening_stock_milli: Some(stock_milli),
        };
        self.core.product_create(&self.owner_token, req).unwrap().row.product_id
    }

    pub fn open_shift(&self, token: &str, float: i64) {
        self.core.shift_open(token, float, &op()).unwrap();
    }

    pub fn user(&self, name: &str, role: &str, pin: &str) -> (String, String) {
        let u = self
            .core
            .user_create(
                &self.owner_token,
                amwapos_core::users::UserInput { display_name: name.into(), role_id: role.into(), pin: Some(pin.into()), active: true },
            )
            .unwrap();
        let t = self.core.login(&u.user_id, pin).unwrap().token;
        (u.user_id, t)
    }
}
