// Types mirroring the Rust command contracts (crates/amwapos-core).
// Money: integer minor units (fils). Quantities: integer thousandths.

export type ErrorCode =
  | "validation"
  | "not_found"
  | "unauthenticated"
  | "forbidden"
  | "approval_required"
  | "invalid_credentials"
  | "account_locked"
  | "conflict"
  | "idempotency_mismatch"
  | "operation_in_progress"
  | "insufficient_stock"
  | "not_set_up"
  | "shift_required"
  | "database_busy"
  | "database_corrupt"
  | "database"
  | "io"
  | "printer"
  | "sync"
  | "duplicate"
  | "insufficient_disk"
  | "ocr_model_missing"
  | "AI_NOT_ENABLED"
  | "AI_NO_KEY"
  | "AI_PROVIDER_ERROR"
  | "AI_TIMEOUT"
  | "AI_MODEL_NOT_FOUND"
  | "internal"
  | "transport";

export interface AppErrorShape {
  code: ErrorCode;
  message: string;
  data_changed: boolean;
  retryable: boolean;
  details?: Record<string, unknown>;
}

export interface DeviceIdentity {
  device_id: string;
  device_code: string;
  name: string;
  branch_id: string;
  mode: "standalone" | "hub" | "terminal";
}

export interface SetupStatus {
  setup_complete: boolean;
  business_name: string | null;
  device: DeviceIdentity | null;
  schema_version: number;
  app_version: string;
  data_dir: string;
  safety_backup: string | null;
}

export interface LoginUser {
  user_id: string;
  display_name: string;
  role_name: string;
  locked: boolean;
}

export interface Session {
  user_id: string;
  display_name: string;
  role_id: string;
  role_name: string;
  permissions: string[];
  device_id: string;
  branch_id: string;
  created_at: string;
  last_activity: string;
  locked: boolean;
}

export interface TenderConfig {
  method: string;
  label: string;
  enabled: boolean;
  requires_reference: boolean;
  allows_change: boolean;
}

export interface PosSettings {
  allow_negative_stock: boolean;
  allow_custom_item: boolean;
  cashier_max_discount_bp: number;
  idle_lock_minutes: number;
  receipt_auto_print: boolean;
  return_to_scan_seconds: number;
  scan_sound: boolean;
  duplicate_scan_window_ms: number;
}

export interface FeatureFlags {
  hub: boolean;
  "whatsapp.enabled": boolean;
  "whatsapp.send_receipts": boolean;
  "whatsapp.delivery_notices": boolean;
  "ocr.enabled": boolean;
  "ocr.payment_screenshots": boolean;
  "ocr.supplier_invoices": boolean;
  "ocr.ai_parse": boolean;
  "ai.enabled": boolean;
  "ai.mutations": boolean;
  "ai.dual_control": boolean;
  "customers.credit": boolean;
  windows_hello: boolean;
  pdf_receipts: boolean;
  updates: boolean;
  "inventory.locations": boolean;
  "loyalty.enabled": boolean;
  "orders.digital": boolean;
  "orders.whatsapp_ai": boolean;
  "orders.whatsapp_upsell": boolean;
  "org.multi_branch": boolean;
  "pwa.companion": boolean;
}

export type FeatureName = keyof FeatureFlags;

export interface PosConfig {
  pos: PosSettings;
  features: FeatureFlags;
  payments: TenderConfig[];
  shift: { blind_close: boolean };
  appearance: { theme: string; density: string; cashier_font: string; scale?: string };
  printer_configured: boolean;
  currency: string;
  currency_digits: number;
  business_name: string;
  timezone: string;
  device: DeviceIdentity | null;
}

export interface Totals {
  subtotal_minor: number;
  discount_minor: number;
  tax_minor: number;
  total_minor: number;
  item_count_milli: number;
}

export interface CartLine {
  line_id: string;
  line_no: number;
  image_hash?: string | null;
  product_id: string | null;
  name: string;
  sku: string | null;
  barcode: string | null;
  unit: string;
  qty_milli: number;
  allow_decimal_quantity: boolean;
  catalog_unit_price_minor: number;
  unit_price_minor: number;
  price_overridden: boolean;
  line_discount_bp: number;
  gross_minor: number;
  discount_minor: number;
  tax_minor: number;
  line_total_minor: number;
  tax_rate_bp: number;
  tax_inclusive: boolean;
  is_custom: boolean;
  stock_milli: number | null;
  /** Set when stock is tracked; a hint shows when stock is at or below it. */
  reorder_point_milli?: number | null;
}

/** Bahrain address parts; `address` is composed from them when given. */
export interface AddressParts {
  flat?: string | null;
  building?: string | null;
  road?: string | null;
  block?: string | null;
  landmark?: string | null;
}

export interface CustomerRef {
  customer_id: string;
  name: string;
  phone: string | null;
  address?: string | null;
  area?: string | null;
  address_parts?: AddressParts | null;
}

export interface Cart {
  cart_id: string | null;
  status: string;
  customer: CustomerRef | null;
  lines: CartLine[];
  totals: Totals;
  cart_discount_minor: number;
  cart_discount_bp: number;
  hold_number: number | null;
  hold_note: string | null;
  version: number;
  notices: string[];
  /** Present when loyalty is on and a customer is on the sale. */
  loyalty?: CartLoyalty | null;
  /** The digital order this sale rings up (Send prefill). */
  order?: {
    order_id: string;
    order_number: string;
    channel: string;
    delivery_wanted: boolean;
    address: string | null;
    phone: string | null;
  } | null;
}

/** What happens to the goods after PAY. */
export interface Fulfilment {
  mode: "here" | "send";
  address?: string | null;
  area?: string | null;
  address_parts?: AddressParts | null;
  phone?: string | null;
  save_on_customer?: boolean;
  notes?: string | null;
  channel?: string | null;
}

export type PayState = "unpaid" | "recorded" | "screenshot_pending" | "paid";

/** A ticket (a sent sale or a digital order) and its drop. */
export interface TicketRow {
  kind: "drop" | "order";
  ticket_id: string;
  delivery_id: string | null;
  order_id: string | null;
  sale_id: string | null;
  number: string;
  delivery_number: string | null;
  customer_id: string | null;
  customer_name: string | null;
  phone: string | null;
  area: string | null;
  address: string | null;
  amount_minor: number;
  outstanding_minor: number;
  pay_state: PayState;
  status: "pending" | "preparing" | "dispatched" | "delivered" | "cancelled" | "draft" | "confirmed";
  channel: string | null;
  assigned_user_id: string | null;
  assigned_name: string | null;
  branch_id: string | null;
  created_at: string;
  updated_at: string;
  delivered_at: string | null;
  problem: "not_delivered" | "unpaid_out" | "notice_failed" | null;
  failed_note?: string | null;
  outcome?: "not_delivered" | null;
  /** Rider still holding cash collected for this ticket (not handed over). */
  cash_with?: string | null;
}

/** Cash a rider holds or still owes, for the till's hand-over. */
export interface RiderCash {
  rider_user_id: string;
  name: string;
  held: {
    collection_id: string;
    delivery_id: string | null;
    number: string | null;
    customer_name: string | null;
    area: string | null;
    amount_minor: number;
    at: string;
  }[];
  held_minor: number;
  uncollected: {
    delivery_id: string;
    number: string;
    customer_name: string | null;
    area: string | null;
    status: string;
    outstanding_minor: number;
  }[];
  uncollected_minor: number;
}

export interface RiderHandover {
  handover_id: string;
  handover_number: string;
  rider_user_id: string;
  rider_name: string | null;
  expected_minor: number;
  counted_minor: number;
  variance_minor: number;
  drops: number;
  at: string;
}

export interface TicketFilter {
  tab?: "now" | "out" | "done" | "board";
  area?: string | null;
  rider?: string | null;
  pay_state?: string | null;
  channel?: string | null;
  customer_id?: string | null;
}

/** What a person accepts when confirming despite a warning. */
export interface ConfirmAck {
  acknowledge_shortage?: boolean;
  acknowledge_price_change?: boolean;
}

/** Counts for the order journey bar; null = not this person's step. */
export interface OrderFlow {
  chats: number | null;
  waiting: number | null;
  to_confirm: number | null;
  to_pack: number | null;
  out: number | null;
  payments: number | null;
}

export interface TicketCounts {
  badge: number;
  now: number;
  out: number;
  done: number;
}

export interface TicketSheet {
  ticket: TicketRow;
  lines: { name: string; qty_milli: number; line_total_minor: number }[];
  events: { from: string | null; to: string; note: string | null; at: string; user: string | null }[];
  payments: { method: string; amount_minor: number; reference: string | null }[];
  collections: {
    method: string;
    amount_minor: number;
    reference: string | null;
    at: string;
    user: string | null;
    held_by?: string | null;
    handed_over?: boolean;
  }[];
  reviews: { review_id: string; review_number: string; status: string; detected_minor: number | null; at: string }[];
  notices: { message_id: string; kind: string; status: string; error: string | null; at: string }[];
  next: string[];
  riders: { user_id: string; name: string }[];
  can: {
    advance: boolean;
    cancel: boolean;
    assign: boolean;
    record_payment: boolean;
    attach_screenshot: boolean;
    message: boolean;
    ring_up: boolean;
    undo: boolean;
    unable?: boolean;
    not_delivered?: boolean;
  };
}

export interface WaThreadContext {
  chat: string;
  phone: string | null;
  push_name: string | null;
  customer: {
    customer_id: string;
    name: string;
    phone: string | null;
    area: string | null;
    address: string | null;
  } | null;
  match: "linked" | "number" | null;
  last_ticket: TicketRow | null;
  tickets: TicketRow[];
  orders_digital: boolean;
}

export interface CartLoyalty {
  balance: number;
  points: number;
  discount_minor: number;
  earn_estimate: number;
  redeem_minor_per_point: number;
  min_redeem_points: number;
}

export interface ScanResult {
  outcome: "added" | "unknown" | "inactive";
  barcode: string;
  product_name: string | null;
  line_id: string | null;
  cart: Cart;
}

export interface PosSearchRow {
  product_id: string;
  sku: string;
  name: string;
  name_ar: string | null;
  category_name: string | null;
  primary_barcode: string | null;
  price_minor: number | null;
  stock_milli: number;
  track_inventory: boolean;
  unit: string;
  stock_status: string;
  image_hash?: string | null;
}

export interface HeldCart {
  cart_id: string;
  hold_number: number | null;
  held_at: string | null;
  user_id: string;
  cashier_name: string;
  customer_name: string | null;
  item_count_milli: number;
  total_minor: number;
  note: string | null;
  locked: boolean;
}

export interface TenderInput {
  method: string;
  amount_minor: number;
  reference?: string | null;
}

export interface PrintOutcome {
  status: "printed" | "failed" | "queued" | "disabled";
  message: string | null;
  job_id: string | null;
}

export interface PaymentView {
  method: string;
  amount_minor: number;
  tendered_minor: number;
  change_minor: number;
  reference: string | null;
}

export interface SaleResult {
  sale_id: string;
  receipt_number: string;
  total_minor: number;
  paid_minor: number;
  change_minor: number;
  payments: PaymentView[];
  completed_at: string;
  replayed: boolean;
  print: PrintOutcome | null;
  stock_warnings?: string[];
  /** The drop created by a Send sale. */
  delivery_id?: string | null;
}

export interface SaleItem {
  sale_item_id: string;
  line_no: number;
  product_id: string | null;
  name: string;
  sku: string | null;
  barcode: string | null;
  unit: string;
  qty_milli: number;
  original_unit_price_minor: number;
  unit_price_minor: number;
  gross_minor: number;
  discount_minor: number;
  tax_rate_bp: number;
  tax_inclusive: boolean;
  tax_minor: number;
  line_total_minor: number;
  cost_minor: number | null;
  refunded_qty_milli: number;
  is_custom: boolean;
}

export interface SaleDetail {
  sale_id: string;
  receipt_number: string;
  status: string;
  completed_at: string;
  business_date: string;
  cashier_user_id: string;
  cashier_name: string;
  device_id: string;
  device_name: string | null;
  shift_id: string;
  customer_id: string | null;
  customer_name: string | null;
  customer_phone: string | null;
  subtotal_minor: number;
  discount_minor: number;
  tax_minor: number;
  total_minor: number;
  paid_minor: number;
  change_minor: number;
  cost_total_minor: number | null;
  items: SaleItem[];
  payments: PaymentView[];
  refunds: { refund_id: string; refund_receipt_number: string; total_minor: number; created_at: string }[];
}

export interface SaleRow {
  sale_id: string;
  receipt_number: string;
  completed_at: string;
  cashier_name: string;
  device_name: string | null;
  customer_name: string | null;
  item_count_milli: number;
  total_minor: number;
  methods: string;
  refunded_minor: number;
  status: string;
}

export interface Page<T> {
  rows: T[];
  total: number;
  limit: number;
  offset: number;
}

export interface RefundPreviewLine {
  sale_item_id: string;
  name: string;
  qty_milli: number;
  amount_minor: number;
  tax_minor: number;
  restock: boolean;
}

export interface RefundTender {
  method: string;
  amount_minor: number;
  reference?: string | null;
}

export interface RefundPreview {
  lines: RefundPreviewLine[];
  subtotal_minor: number;
  tax_minor: number;
  total_minor: number;
  tenders: RefundTender[];
  requires_approval: boolean;
}

export interface RefundResult {
  refund_id: string;
  refund_receipt_number: string;
  total_minor: number;
  tax_minor: number;
  tenders: RefundTender[];
  created_at: string;
  replayed: boolean;
  print: PrintOutcome | null;
}

export interface MethodTotal {
  method: string;
  amount_minor: number;
  count: number;
}

export interface ShiftSummary {
  shift_id: string;
  shift_number: string;
  user_id: string;
  cashier_name: string;
  device_id: string;
  device_name: string | null;
  status: "open" | "closed";
  business_date: string;
  opened_at: string;
  closed_at: string | null;
  opening_float_minor: number;
  sale_count: number;
  sales_total_minor: number;
  discount_total_minor: number;
  tax_total_minor: number;
  by_method: MethodTotal[];
  refund_count: number;
  refunds_total_minor: number;
  cash_sales_minor: number;
  cash_refunds_minor: number;
  paid_in_minor: number;
  paid_out_minor: number;
  safe_drop_minor: number;
  /** Cash collected on this shift for pay-on-delivery tickets. */
  cash_collections_minor?: number;
  rider_handover_minor?: number;
  no_sale_count: number;
  expected_cash_minor: number;
  counted_cash_minor: number | null;
  variance_minor: number | null;
  expected_visible: boolean;
  close_note: string | null;
  variance_approved_by_name: string | null;
}

export interface ProductRow {
  product_id: string;
  sku: string;
  name: string;
  name_ar: string | null;
  category_id: string | null;
  category_name: string | null;
  primary_barcode: string | null;
  barcode_count: number;
  price_minor: number | null;
  cost_minor: number | null;
  stock_milli: number;
  reorder_point_milli: number;
  unit: string;
  track_inventory: boolean;
  allow_decimal_quantity: boolean;
  active: boolean;
  is_favorite: boolean;
  tax_rule_id: string;
  tax_rate_bp: number;
  tax_inclusive: boolean;
  stock_status: string;
  /** Stored product image (content hash; fetched through `products.images`). */
  image_hash?: string | null;
  image_source?: "manual" | "automatic" | null;
  auto_image_status?: AutoImageStatus;
}

export type AutoImageStatus = "not_attempted" | "pending" | "processing" | "found" | "not_found" | "failed" | "skipped";

export interface ProductImageState {
  product_id: string;
  image_hash: string | null;
  image_source: "manual" | "automatic" | null;
  auto_image_status: AutoImageStatus;
  auto_image_attempted_at: string | null;
  auto_image_note: Record<string, unknown> | null;
  /** Whether automatic discovery can run right now. */
  discovery: DiscoveryAvailability;
}

export type DiscoveryAvailability = "active" | "switched_off" | "disabled_by_administrator" | "no_sources";

export interface ImageSearchSettings {
  /** Find pictures automatically for new products (on by default). */
  enabled: boolean;
  /** Default: barcode + name on Bing's thumbnail address (tse1.mm.bing.net/th?q=…). */
  bing_thumbnail: boolean;
  open_food_facts: boolean;
  bing: boolean;
  google: boolean;
  google_cx: string;
  region: string;
  language: "en" | "ar";
}

export interface ImageOverview {
  enabled: boolean;
  availability: DiscoveryAvailability;
  /** AMWAPOS_IMAGE_SEARCH=off on this computer. */
  environment_disabled: boolean;
  /** Google is switched on and has its engine id and key. */
  google_ready: boolean;
  sources: { bing_thumbnail?: boolean; open_food_facts: boolean; bing: boolean; google: boolean };
  counts: Partial<Record<AutoImageStatus, number>>;
  with_image: number;
  never_searched: number;
  settings: ImageSearchSettings;
  google_key_set: boolean;
}

export interface BarcodeRow {
  barcode_id: string;
  barcode: string;
  is_primary: boolean;
  source: string;
  created_at: string;
}

export interface PriceRow {
  price_id: string;
  amount_minor: number;
  effective_from: string;
  effective_to: string | null;
  reason: string | null;
  created_by_name: string | null;
  created_at: string;
}

export interface CostRow {
  cost_id: string;
  cost_minor: number;
  source: string;
  supplier_name: string | null;
  effective_at: string;
  created_by_name: string | null;
}

export interface ProductDetail extends ProductRow {
  description: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  archived_at: string | null;
  barcodes: BarcodeRow[];
  price_history: PriceRow[];
  cost_history: CostRow[] | null;
  avg_cost_minor: number | null;
  last_cost_minor: number | null;
}

export interface ProductInput {
  sku?: string | null;
  name: string;
  name_ar?: string | null;
  description?: string | null;
  category_id?: string | null;
  tax_rule_id: string;
  unit: string;
  track_inventory: boolean;
  allow_decimal_quantity: boolean;
  reorder_point_milli: number;
  is_favorite: boolean;
}

export interface CategoryRow {
  category_id: string;
  parent_id: string | null;
  name: string;
  sort_order: number;
  active: boolean;
  product_count: number;
}

export interface TaxRuleRow {
  tax_rule_id: string;
  name: string;
  rate_bp: number;
  inclusive: boolean;
  active: boolean;
  effective_from: string;
  product_count: number;
}

export interface UnknownBarcodeRow {
  barcode: string;
  first_seen_at: string;
  last_seen_at: string;
  scan_count: number;
  last_device_name: string | null;
  last_user_name?: string | null;
  status: string;
  resolved_product_id: string | null;
  resolved_product_name: string | null;
}

export interface MovementRow {
  movement_id: string;
  created_at: string;
  product_id: string;
  product_name: string;
  sku: string;
  kind: string;
  qty_delta_milli: number;
  balance_after_milli: number;
  unit_cost_minor: number | null;
  source_type: string;
  source_id: string | null;
  source_ref: string | null;
  reason: string | null;
  user_name: string | null;
}

export interface StocktakeRow {
  stocktake_id: string;
  stocktake_number: string;
  name: string;
  scope_type: string;
  status: "counting" | "review" | "completed" | "cancelled";
  blind: boolean;
  created_at: string;
  created_by_name: string | null;
  line_count: number;
  counted_count: number;
  variance_lines: number;
  finalized_at: string | null;
}

export interface StocktakeLine {
  product_id: string;
  sku: string;
  name: string;
  primary_barcode: string | null;
  unit: string;
  allow_decimal_quantity: boolean;
  expected_qty_milli: number | null;
  system_qty_at_count_milli: number | null;
  counted_qty_milli: number | null;
  variance_milli: number | null;
  unit_cost_minor: number | null;
  counted_at: string | null;
}

export interface StocktakeDetail extends StocktakeRow {
  lines: StocktakeLine[];
  expected_value_minor: number | null;
  counted_value_minor: number | null;
  variance_value_minor: number | null;
}

export interface SupplierInput {
  name: string;
  cr_number?: string | null;
  vat_number?: string | null;
  contact_name?: string | null;
  phone?: string | null;
  whatsapp?: string | null;
  email?: string | null;
  address?: string | null;
  payment_terms?: string | null;
  notes?: string | null;
  active: boolean;
}

export interface SupplierRow extends SupplierInput {
  supplier_id: string;
  open_po_count: number;
  last_purchase_at: string | null;
  created_at: string;
}

export interface PoRow {
  po_id: string;
  po_number: string;
  supplier_id: string;
  supplier_name: string;
  status: "draft" | "ordered" | "partially_received" | "received" | "cancelled";
  reference: string | null;
  ordered_at: string | null;
  expected_at: string | null;
  created_at: string;
  line_count: number;
  total_minor: number;
  received_pct: number;
}

export interface PoLine {
  po_item_id: string;
  line_no: number;
  product_id: string;
  product_name: string;
  sku: string;
  primary_barcode: string | null;
  allow_decimal_quantity: boolean;
  qty_ordered_milli: number;
  qty_received_milli: number;
  qty_remaining_milli: number;
  unit_cost_minor: number;
  tax_rate_bp: number;
  total_minor: number;
}

export interface PoDetail extends PoRow {
  notes: string | null;
  subtotal_minor: number;
  tax_minor: number;
  version: number;
  lines: PoLine[];
  receipts: {
    receipt_id: string;
    reference: string | null;
    total_cost_minor: number;
    created_at: string;
    user_name: string | null;
  }[];
}

export interface CustomerInput {
  name: string;
  phone?: string | null;
  whatsapp?: string | null;
  email?: string | null;
  area?: string | null;
  address?: string | null;
  address_parts?: AddressParts | null;
  active: boolean;
}

export interface CustomerRow extends CustomerInput {
  customer_id: string;
  created_at: string;
  purchase_count: number;
  total_spent_minor: number;
  last_purchase_at: string | null;
  /** Present when loyalty is on. */
  loyalty_points?: number;
}

export interface DeliveryRow {
  delivery_id: string;
  delivery_number: string;
  sale_id: string | null;
  receipt_number: string | null;
  customer_id: string | null;
  customer_name: string | null;
  phone: string | null;
  area: string | null;
  address: string | null;
  status: "pending" | "preparing" | "dispatched" | "delivered" | "cancelled";
  payment_status: "paid" | "pending" | "cod";
  amount_minor: number;
  assigned_user_id: string | null;
  assigned_name: string | null;
  notes: string | null;
  created_at: string;
  dispatched_at: string | null;
  delivered_at: string | null;
}

export interface ReportColumn {
  key: string;
  label: string;
  kind: "text" | "money" | "qty" | "int" | "percent_bp" | "datetime" | "date";
}

export interface Kpi {
  label: string;
  value: number;
  kind: string;
  previous: number | null;
}

export interface Report {
  key: string;
  title: string;
  from: string;
  to: string;
  kpis: Kpi[];
  columns: ReportColumn[];
  rows: Record<string, unknown>[];
  totals: Record<string, unknown> | null;
  series: { label: string; value: number }[] | null;
  notes: string[];
}

export interface ReportParams {
  from?: string;
  to?: string;
  group_by?: string;
  category_id?: string;
  cashier_id?: string;
  limit?: number;
  days?: number;
  branch_id?: string;
}

export interface UserRow {
  user_id: string;
  display_name: string;
  role_id: string;
  role_name: string;
  active: boolean;
  last_login_at: string | null;
  locked_until: string | null;
  failed_attempts: number;
  created_at: string;
}

export interface RoleRow {
  role_id: string;
  name: string;
  description: string | null;
  is_system: boolean;
  user_count: number;
  permissions: string[];
}

export interface PermissionRow {
  code: string;
  domain: string;
  description: string;
}

export interface AuditRow {
  seq: number;
  audit_id: string;
  created_at: string;
  user_name: string | null;
  approver_name: string | null;
  device_name: string | null;
  event_type: string;
  entity_type: string;
  entity_id: string | null;
  before: unknown;
  after: unknown;
  previous_hash: string;
  audit_hash: string;
}

export interface DiagnosticItem {
  component: string;
  state: "ok" | "warning" | "error" | "info";
  summary: string;
  details: Record<string, unknown>;
}

export interface DeviceRow {
  device_id: string;
  name: string;
  device_code: string;
  mode: string;
  branch_name: string | null;
  active: boolean;
  activated_at: string;
  revoked_at: string | null;
  app_version: string | null;
  last_seen_at: string | null;
  pending_count: number | null;
  last_error: string | null;
  is_this_device: boolean;
}

export interface BackupRow {
  backup_id: string | null;
  path: string;
  file_name: string;
  kind: string;
  size_bytes: number | null;
  status: string;
  error: string | null;
  created_at: string;
  duration_ms: number | null;
  exists: boolean;
}

export interface BackupInspection {
  path: string;
  ok: boolean;
  integrity: string;
  schema_version: number;
  current_schema_version: number;
  compatible: boolean;
  business_name: string | null;
  same_business: boolean;
  record_counts: Record<string, number>;
  sha256: string;
  manifest_sha256: string | null;
  checksum_matches: boolean | null;
  size_bytes: number;
  created_at: string | null;
  problems: string[];
}

export interface ImportRow {
  row: number;
  action: "create" | "update" | "error";
  errors: string[];
  warnings: string[];
  sku: string | null;
  name: string;
  barcodes: string[];
  price_minor: number | null;
  cost_minor: number | null;
  category: string | null;
}

export interface ImportPreview {
  columns: string[];
  mapping: Record<string, string>;
  fields: { key: string; label: string }[];
  total_rows: number;
  creates: number;
  updates: number;
  errors: number;
  warnings: number;
  barcodes_added: number;
  new_categories: string[];
  rows: ImportRow[];
  spreadsheet_warning: string | null;
}

export interface PrintJobRow {
  job_id: string;
  kind: string;
  ref_id: string | null;
  reference: string | null;
  status: string;
  attempts: number;
  last_error: string | null;
  created_at: string;
}

// ---- WhatsApp (in-process) / OCR worker ----

export interface FileBlob {
  mime: string;
  base64: string;
  size: number;
}

/** In-process WhatsApp service: each flag separate. */
export interface WaStatus {
  enabled: boolean;
  process: "disabled" | "stopped" | "starting" | "running" | "restarting" | "failed";
  session: "none" | "pairing" | "paired" | "logged_out";
  connected: boolean;
  ready: boolean;
  account: string | null;
  qr: { svg: string | null; expires_at: string } | null;
  pair_code: { code: string; expires_at: string } | null;
  last_error: string | null;
  restarts: number;
  next_retry_at: string | null;
  banned_until: string | null;
  adapter: string;
  session_file: string;
  inbox_rev: number;
  last_send_at: string | null;
  last_send_error: string | null;
  /** Last time contacts saved on the phone arrived from WhatsApp. */
  contacts_synced_at?: string | null;
}

export interface OcrWorkerStatus {
  available: boolean;
  languages: string[];
  engine: string | null;
  error_code: "ocr_model_missing" | "ocr_engine_missing" | null;
  error: string | null;
  running: boolean;
  last_job_at: string | null;
}

export interface AutomationStatus {
  whatsapp: WaStatus;
  ocr: OcrWorkerStatus;
  queue: { unread: number; queued: number; failed: number } | null;
  features: Record<
    | "whatsapp.enabled"
    | "whatsapp.send_receipts"
    | "whatsapp.delivery_notices"
    | "ocr.enabled"
    | "ocr.payment_screenshots"
    | "ocr.supplier_invoices",
    boolean
  >;
}

export interface WaRecent {
  sent: {
    message_id: string;
    operation_id: string;
    kind: string;
    status: string;
    wa_message_id: string | null;
    to_phone: string;
    created_at: string;
    sent_at: string | null;
    last_error: string | null;
  }[];
  received: {
    seq: number;
    wa_id: string;
    chat: string;
    kind: string;
    received_at: string;
    media_state: string;
    media_error: string | null;
  }[];
}

export interface WaQueueRequest {
  operation_id: string;
  kind: "receipt" | "received" | "dispatch" | "delivered" | "reminder" | "payment_ack" | "text" | "document";
  to_phone?: string | null;
  customer_id?: string | null;
  sale_id?: string | null;
  delivery_id?: string | null;
  review_id?: string | null;
  lang?: "en" | "ar" | null;
  text?: string | null;
  document_b64?: string | null;
  document_name?: string | null;
}

export interface WaOutboxRow {
  message_id: string;
  kind: string;
  to_phone: string;
  customer_id: string | null;
  customer_name: string | null;
  sale_id: string | null;
  delivery_id: string | null;
  lang: string;
  body: string;
  document_name: string | null;
  status: "queued" | "sending" | "sent" | "failed" | "cancelled";
  attempts: number;
  last_error: string | null;
  created_by_name: string | null;
  created_at: string;
  sent_at: string | null;
}

export interface WaConversation {
  chat: string;
  phone: string | null;
  name: string | null;
  customer_id: string | null;
  last_at: string;
  last_text: string | null;
  unread: number;
}

export interface WaInboxRow {
  seq: number;
  chat: string;
  phone: string | null;
  push_name: string | null;
  customer_id: string | null;
  customer_name: string | null;
  received_at: string;
  kind: "text" | "image" | "document" | "other";
  body: string | null;
  caption: string | null;
  has_media: boolean;
  media_mime: string | null;
  read_at: string | null;
}

export interface WaThread {
  chat: string;
  phone: string | null;
  inbound: WaInboxRow[];
  outbound: WaOutboxRow[];
}

export interface PaymentReview {
  review_id: string;
  review_number: string;
  source: "whatsapp" | "upload";
  inbox_seq: number | null;
  phone: string | null;
  customer_id: string | null;
  customer_name: string | null;
  sale_id: string | null;
  delivery_id: string | null;
  delivery_number: string | null;
  expected_minor: number | null;
  detected_minor: number | null;
  detected_reference: string | null;
  ocr_confidence: number | null;
  ocr_text: string | null;
  duplicate_of: string | null;
  ocr_status: "ocr_match" | "likely_match" | "mismatch" | "needs_review" | null;
  status: "pending" | "ocr_match" | "likely_match" | "mismatch" | "needs_review" | "confirmed" | "rejected";
  reason: string | null;
  decided_by_name: string | null;
  decided_at: string | null;
  note: string | null;
  created_at: string;
  /** E4: expected vs detected, check by check. Never settles anything by itself. */
  comparison?: {
    expected_minor: number | null;
    detected_minor: number | null;
    difference_minor: number | null;
    verdict: "exact" | "overpaid" | "underpaid" | "amount_not_read" | "no_expected_amount";
    all_checks_pass: boolean;
    checks: { check: "amount" | "reference" | "ocr_confidence" | "not_duplicate"; ok: boolean; detail: unknown }[];
    settles_automatically: false;
  };
}

export interface InvoiceScan {
  scan_id: string;
  scan_number: string;
  supplier_id: string | null;
  supplier_name: string | null;
  file_name: string | null;
  status: "imported" | "read" | "review" | "confirmed" | "rejected" | "failed";
  ocr_confidence: number | null;
  invoice_number: string | null;
  invoice_date: string | null;
  total_minor: number | null;
  lines_total_minor: number;
  po_id: string | null;
  po_number: string | null;
  error: string | null;
  duplicate_of: string | null;
  created_by_name: string | null;
  created_at: string;
}

export interface InvoiceScanLine {
  line_no: number;
  raw_text: string;
  description: string | null;
  code: string | null;
  qty_milli: number | null;
  unit_cost_minor: number | null;
  line_total_minor: number | null;
  product_id: string | null;
  product_name: string | null;
  current_cost_minor: number | null;
  match_kind: "barcode" | "sku" | "name" | "manual" | "none";
  match_score: number;
  include: boolean;
}

export interface InvoiceScanDetail {
  scan: InvoiceScan;
  lines: InvoiceScanLine[];
  ocr_text: string | null;
  image: FileBlob | null;
}

// ---- AI assistant ----

/** Bring your own API key: consumer subscriptions cannot be signed into. */
export type AiProvider = "fake" | "openai" | "anthropic" | "google" | "openrouter" | "custom";

export interface AiSettings {
  provider: AiProvider;
  model_id: string;
  base_url: string;
  extra_header_name: string;
  max_output_tokens: number;
  timeout_ms: number;
  list_models_cache: string[];
  fallbacks: boolean;
  consent: boolean;
  consent_by?: string | null;
  consent_at?: string | null;
  /** Input + output tokens per business day; 0 = no cap. */
  daily_token_cap: number;
  /** When the provider is unavailable, answer with OpenRouter (owner opt-in). */
  fallback_free: boolean;
  fallback_model: string;
  fallback_base_url: string;
  fallback_consent_at?: string | null;
  /** A9: "ui" follows the screen language. */
  answer_language?: "ui" | "en" | "ar";
  /** C1: faster model for sorting messages and short drafts; empty = main model. */
  model_id_fast?: string;
  /** B5: target margin on cost in basis points (2500 = 25 %). */
  target_margin_bp?: number;
  /** B5: suggested prices round up to this many minor units. */
  price_round_minor?: number;
  /** B3 thresholds. */
  anomaly_refund_count?: number;
  anomaly_refund_minor?: number;
  anomaly_discount_minor?: number;
  anomaly_hub_lag_minutes?: number;
}

export interface AiAlert {
  alert_id: string;
  kind: "refund_spike" | "discount_spike" | "negative_stock" | "hub_lag" | "backup_overdue";
  day: string;
  severity: "info" | "warning" | "danger";
  title: string;
  detail: Record<string, unknown> | null;
  created_at: string;
  dismissed_at: string | null;
}

export interface AiStatus {
  /** Full settings for the owner; provider and model only for others. */
  settings: Partial<AiSettings> & { provider: AiProvider; model_id: string };
  key_configured: boolean;
  extra_header_configured: boolean;
  /** "fake" whenever no key is stored for the selected provider. */
  active_provider: AiProvider;
  model_id: string;
  enabled: boolean;
  mutations: boolean;
  can_mutate: boolean;
  is_owner: boolean;
  ready: boolean;
  tokens_today?: number;
  daily_token_cap?: number;
  /** Set after a save that moved between two real providers: consent must be given again. */
  consent_reset?: boolean;
  fallback_key_configured?: boolean;
  fallback_ready?: boolean;
}

export interface AiTestResult {
  ok: boolean;
  status: number | null;
  models?: number;
  model_listed?: boolean;
  code?: string;
  error?: string;
}

export interface AiProposal {
  proposal_id: string;
  proposal_number: string;
  conversation_id: string;
  /** The three original kinds, or "command:<admin command>" for the full tool map. */
  kind: "price_change" | "stock_adjustment" | "purchase_order" | `command:${string}`;
  params: Record<string, unknown>;
  preview: Record<string, unknown>;
  risk: "low" | "medium" | "high";
  risk_reasons: string[];
  status: "proposed" | "executing" | "executed" | "rejected" | "failed" | "undone" | "expired";
  result: Record<string, unknown> | null;
  error: string | null;
  created_at: string;
  decided_by_name: string | null;
  decided_at: string | null;
  undone_at: string | null;
  /** Two-person control: who confirmed first. */
  first_confirmed_by_name?: string | null;
}

export interface AiConversation {
  conversation_id: string;
  title: string;
  untrusted_seen: boolean;
  messages: {
    role: "user" | "assistant";
    text: string;
    tools: string[];
    at: string;
    stop_reason: string | null;
    /** Tool calls behind this answer (tool, ids it was called with, when). */
    evidence?: { tool: string; ids: string[]; at: string }[];
    /** The answer stated figures without a tool result behind them. */
    unverified?: boolean;
    /** Every tool call in this reply, with the result exactly as the model saw it. */
    calls?: AiToolCall[];
    /** Thinking or reasoning text the provider returned (summarised). */
    thinking?: string;
    attachments?: { attachment_id: string; media_type: string }[];
    has_context?: boolean;
    /** "nudge": AMWAPOS asked the model to back its figures with a tool. */
    kind?: "nudge";
  }[];
  proposals: AiProposal[];
  pins?: AiPin[];
}

export interface AiToolCall {
  id: string;
  name: string;
  input: unknown;
  result: string;
  is_error: boolean;
}

export interface AiPin {
  kind: "product" | "customer" | "supplier" | "order" | "shift" | "po" | "sale" | "delivery";
  id: string;
  label: string;
}

/** One live event of a question (C8). */
export type AiStreamEvent = { seq: number } & (
  | { type: "start"; conversation_id: string; provider: string; model: string; tools: number }
  | { type: "round"; n: number }
  | { type: "text"; delta: string }
  | { type: "thinking"; delta: string }
  | { type: "tool_call"; id: string; name: string; input: unknown }
  | { type: "tool_result"; id: string; name: string; is_error: boolean; content: string; truncated: boolean }
  | { type: "usage"; input_tokens: number; output_tokens: number; stop_reason: string }
  | { type: "fallback"; from: string; to: string; reason: string }
  | { type: "nudge" }
  | { type: "unverified" }
  | { type: "done" }
  | { type: "error"; code: string; message: string }
);

export interface AiContext {
  kind: "cart";
  lines: {
    product_id?: string | null;
    name: string;
    qty_milli: number;
    unit_price_minor: number;
    line_total_minor: number;
  }[];
  total_minor: number;
  customer?: string | null;
  held_ticket?: string | null;
}

export interface AiBriefing {
  briefing_id: string;
  name: string;
  playbook: "eod" | "cash_short" | "reorder" | "refund_spike";
  at_time: string;
  days: string;
  with_ai: boolean;
  enabled: boolean;
  last_run_on: string | null;
  created_by_name: string | null;
}

export interface AiNote {
  note_id: string;
  briefing_id: string | null;
  title: string;
  summary: string | null;
  data: unknown;
  status: "ok" | "partial" | "error";
  error: string | null;
  created_at: string;
  read_at: string | null;
  created_by_name: string | null;
}

export interface AiSlashResult {
  command: string;
  ran: string;
  result: unknown;
  truncated: boolean;
}

export interface WaTriageItem {
  seq: number;
  chat: string;
  phone: string | null;
  push_name: string | null;
  received_at: string;
  kind: string;
  preview: string;
  read: boolean;
  category: "order" | "payment" | "complaint" | "question" | "spam" | "other";
  confidence: number;
  source: "rules" | "ai" | "person";
  reasons: string[];
  suggestion: { action: string; tool: string | null; link: string };
}

/** Confirm result: the proposal, plus a one-time secret (e.g. a phone-view link) never stored. */
export type AiConfirmResult = AiProposal & { once?: Record<string, unknown> };

export interface AiDigest {
  date: string;
  counts: { status: string; risk: string; count: number }[];
  proposals: AiProposal[];
}

export interface AiPlaybookResult {
  playbook: string;
  date: string;
  steps: { tool: string; ok: boolean; result?: unknown; error?: string }[];
}

export interface MigrationTable {
  source: string;
  headers: string[];
  rows: string[][];
  numeric_columns: string[];
  entity: "products" | "customers" | "suppliers" | "stock" | "unknown";
  mapping: Record<string, string>;
}

export interface UpdateManifest {
  version: string;
  notes: string;
  published_at: string;
  installer: { url: string; sha256: string; size: number; file_name: string };
}

export interface UpdateStatus {
  current_version: string;
  signing_key_built_in: boolean;
  available: UpdateManifest | null;
  downloaded: UpdateManifest | null;
  can_install: boolean;
}

export interface CustomerAddress {
  address_id: string;
  label: string;
  area: string | null;
  address: string;
  notes: string | null;
  is_default: boolean;
}

export interface CustomerAccountView {
  credit_enabled: boolean;
  account: { enabled: boolean; credit_limit_minor: number; balance_minor: number; available_minor: number } | null;
  ledger: {
    entry_id: string;
    kind: "sale" | "payment" | "refund" | "adjustment";
    amount_minor: number;
    method: string | null;
    note: string | null;
    user: string | null;
    created_at: string;
    reference: string | null;
  }[];
  addresses: CustomerAddress[];
}

// ---- Product-brief pillars -------------------------------------------------

export interface LoyaltySettings {
  earn_minor_per_point: number;
  redeem_minor_per_point: number;
  exclude_discounted_lines: boolean;
  min_redeem_points: number;
}

export interface LoyaltyEntry {
  entry_id: string;
  kind: "earn" | "redeem" | "reverse_earn" | "reverse_redeem" | "adjust";
  points: number;
  sale_id: string | null;
  receipt_number: string | null;
  note: string | null;
  user_name: string | null;
  created_at: string;
}

export interface LoyaltyCustomer {
  balance: number;
  value_minor: number;
  entries: LoyaltyEntry[];
  settings: LoyaltySettings;
}

export interface StockLocation {
  location_id: string;
  branch_id: string;
  branch_name: string;
  code: string;
  name: string;
  is_default: boolean;
  active: boolean;
}

export interface TransferLine {
  line_no: number;
  product_id: string;
  product_name: string;
  qty_milli: number;
  qty_received_milli: number;
}

export interface Transfer {
  transfer_id: string;
  transfer_number: string;
  from_branch_id: string;
  from_branch_name: string;
  from_location_id: string;
  from_location_name: string;
  to_branch_id: string;
  to_branch_name: string;
  to_location_id: string;
  to_location_name: string;
  status: "draft" | "shipped" | "received" | "cancelled";
  note: string | null;
  created_by_name: string | null;
  created_at: string;
  shipped_at: string | null;
  received_at: string | null;
  lines: TransferLine[];
}

export type OrderChannel = "phone" | "whatsapp" | "web" | "other";
export type OrderPaymentState = "unpaid" | "recorded" | "screenshot_pending";

export interface OrderLine {
  line_no: number;
  product_id: string | null;
  product_name: string | null;
  description: string;
  qty_milli: number;
  unit_price_minor: number | null;
}

export interface DigitalOrder {
  order_id: string;
  order_number: string;
  branch_id: string;
  channel: OrderChannel;
  external_ref: string | null;
  customer_id: string | null;
  customer_name: string | null;
  phone: string | null;
  status: "draft" | "confirmed" | "converted" | "cancelled";
  payment_state: OrderPaymentState;
  inbox_seq: number | null;
  note: string | null;
  address: string | null;
  delivery_wanted: boolean;
  sale_id: string | null;
  receipt_number: string | null;
  delivery_id: string | null;
  created_by_name: string | null;
  created_at: string;
  updated_at: string;
  lines: OrderLine[];
  estimate_minor: number;
}

export interface OrderInput {
  channel: OrderChannel;
  external_ref?: string | null;
  customer_id?: string | null;
  phone?: string | null;
  payment_state?: OrderPaymentState;
  note?: string | null;
  address?: string | null;
  delivery_wanted?: boolean;
  lines: { product_id?: string | null; description?: string | null; qty_milli: number }[];
}

export interface BranchDevice {
  device_id: string;
  name: string;
  device_code: string;
  mode: string;
  active: boolean;
}

export interface Branch {
  branch_id: string;
  code: string;
  name: string;
  address: string | null;
  phone: string | null;
  active: boolean;
  devices: BranchDevice[];
  user_count: number;
}

export interface BranchPrice {
  branch_id: string;
  code: string;
  name: string;
  price_minor: number | null;
}

export interface EodPack {
  date: string;
  branch_id: string | null;
  sales: Report | null;
  tenders: Report | null;
  shifts: Report | null;
  refunds: Report | null;
  low_stock: { product_id: string; name: string; sku: string; qty_milli: number; reorder_point_milli: number }[];
  rider_cash_held?: { rider_user_id: string; name: string; drops: number; amount_minor: number; since: string }[];
  hidden: string[];
}

export type RangeKind = "today" | "yesterday" | "last_7" | "this_week" | "this_month" | "last_month" | "fixed";

export interface ReportPreset {
  preset_id: string;
  name: string;
  range_kind: RangeKind;
  from_date: string | null;
  to_date: string | null;
}

export interface CompanionToken {
  id: string;
  user_name: string | null;
  label: string | null;
  created_at: string;
  expires_at: string;
  last_used_at: string | null;
}

/** Contacts saved on the linked WhatsApp phone, with what an import would do. */
export interface WaPhoneContact {
  jid: string;
  phone: string | null;
  name: string | null;
  /** Digits with their hyphens/slashes taken from the saved name ("825 - 3325"). */
  address: string | null;
  status: "new" | "exists" | "no_phone" | "no_name";
  customer_id: string | null;
  customer_name: string | null;
  updated_at: string;
}
export interface WaPhoneContacts {
  contacts: WaPhoneContact[];
  counts: { new: number; exists: number; no_phone: number; no_name: number };
  last_sync_at: string | null;
}

/** WhatsApp Business catalogue: what the linked connection can do. */
export type WaCatalogCapability =
  | "disconnected"
  | "checking"
  | "personal"
  | "business_no_catalog"
  | "supported"
  | "unavailable"
  | "unsupported"
  | "terminal";

export type WaCatalogItemStatus =
  "queued" | "syncing" | "synced" | "hidden" | "removed" | "not_synced" | "failed" | "remote_missing";

/** An administrator's full sync, with its progress. */
export interface WaCatalogRun {
  run_id: string;
  started_at: string;
  finished_at: string | null;
  /** Products this run has to write (unchanged ones are not counted). */
  total: number;
  processed: number;
  /** Products already identical on WhatsApp when the run started. */
  unchanged: number;
  synced: number;
  hidden: number;
  removed: number;
  failed: number;
  /** The one comparison with the remote catalogue per run. */
  verify: "pending" | "done" | "skipped";
  verify_note: string | null;
}

export type WaCatalogLeftOut = "archived" | "no_name" | "no_price" | "price_not_supported";

export interface WaCatalogOverview {
  account: string | null;
  published: boolean;
  auto_sync: boolean;
  publishable: number;
  not_publishable: Partial<Record<WaCatalogLeftOut, number>>;
  counts: Partial<Record<WaCatalogItemStatus, number>>;
  /** Waiting after a temporary error; retried automatically. */
  retrying?: number;
  /** On WhatsApp, but changed in AMWAPOS since (auto-sync off). */
  out_of_date?: number;
  last_synced_at: string | null;
  /** `error` is the operator wording; `detail` what WhatsApp said. */
  failures: { product_id: string; name: string | null; status: string; error: string | null; detail?: string | null }[];
  categories: number;
  collections_recorded: number;
  run?: WaCatalogRun | null;
  last_run?: WaCatalogRun | null;
}

export interface WaCatalogStatus {
  capability: {
    capability: WaCatalogCapability;
    detail: string | null;
    account: string | null;
    checked_at: string | null;
    collections: boolean;
    last_pass_at: string | null;
    last_pass_done: number;
    last_error: string | null;
  };
  connection: { connected: boolean; ready: boolean; enabled: boolean };
  catalog: WaCatalogOverview;
}

export interface WaCatalogProductState {
  published: boolean;
  status: WaCatalogItemStatus | null;
  on_whatsapp?: boolean;
  last_synced_at?: string | null;
  last_error?: string | null;
  not_publishable?: WaCatalogLeftOut | null;
  out_of_date?: boolean;
  picture_refused?: boolean;
}

// ---- Document Intelligence (supplier documents → review → drafts)

/** Confidence band shown next to every extracted value. */
export type Band = "high" | "medium" | "low" | "unresolved";

export interface DocEvidence {
  page: number | null;
  line: number | null;
  /** 0..1 of the page: left, top, width, height. */
  bbox: [number, number, number, number] | null;
  text: string | null;
  ocr_conf: number | null;
}

export interface DocField<T = string | number | null> {
  value: T | null;
  raw: string | null;
  evidence: DocEvidence | null;
  /** rules | ai | person | learned */
  source: string;
  band: Band;
  /** ok | missing | conflict | corrected */
  status: string;
  note: string | null;
}

export interface DocCandidate {
  id: string;
  name: string;
  score: number;
  reasons: string[];
}

export interface DocLine {
  line_no: number;
  raw_text: string;
  description: string | null;
  code: string | null;
  barcode: string | null;
  barcode_valid: boolean | null;
  qty_milli: number | null;
  unit: string | null;
  case_qty_milli: number | null;
  units_per_case: number | null;
  base_qty_milli: number | null;
  pack_text: string | null;
  pack_clear: boolean;
  unit_cost_minor: number | null;
  discount_minor: number | null;
  vat_rate_bp: number | null;
  vat_minor: number | null;
  line_total_minor: number | null;
  product_id: string | null;
  product_name: string | null;
  match_kind: string;
  match_score: number;
  match_band: Band;
  candidates: DocCandidate[] | null;
  reasons: string[] | null;
  evidence: DocEvidence | null;
  flags: string[] | null;
  include: boolean;
  new_product: boolean;
  corrected: boolean;
  po_item_id: string | null;
  last_cost_minor: number | null;
}

export interface DocIssue {
  code: string;
  severity: "error" | "warning" | "info";
  message: string;
  line_no?: number;
}

export interface DocReconLine {
  product_id: string | null;
  product_name: string | null;
  line_no: number | null;
  ordered_milli: number | null;
  received_milli: number | null;
  invoiced_milli: number | null;
  po_cost_minor: number | null;
  invoice_cost_minor: number | null;
  cost_variance_minor: number | null;
  cost_variance_pct: string | null;
  states: string[];
  notes: string[];
}

export interface DocDetail {
  scan: InvoiceScan;
  revision: number;
  stage: string;
  mime: string | null;
  source: string;
  inbox_seq: number | null;
  page_count: number | null;
  ai_model: string | null;
  corrections: number;
  classification: { doc_type: string; band: Band | null; source: string | null; reasons: string[] };
  quality: {
    status?: string | null;
    messages?: string[];
    pages?: { page: number; status?: string; messages?: string[] }[];
  };
  fields: Record<string, DocField> | null;
  supplier_match: {
    id: string | null;
    name: string | null;
    kind: string;
    score: number;
    band: Band;
    reasons: string[];
    alternatives: { id: string; name: string; score: number; reasons: string[] }[];
  } | null;
  validation: {
    arithmetic_ok: boolean;
    vat_ok: boolean;
    calc_total_minor: number | null;
    calc_vat_minor: number | null;
    lines_net_minor: number;
    line_basis: string;
    issues: DocIssue[];
  } | null;
  duplicates: { kind: string; scan_id: string; scan_number: string; status: string; reasons: string[] }[] | null;
  anomalies: { code: string; message: string; line_no?: number }[] | null;
  recon: {
    po_id: string | null;
    po_number: string | null;
    po_status: string | null;
    selected_by: string | null;
    candidates: { po_id: string; po_number: string; status: string; score: number; reasons: string[] }[];
    three_way: boolean;
    lines: DocReconLine[];
    summary: string[];
  } | null;
  summary: string | null;
  lines: DocLine[];
  receive: {
    line_no: number;
    receive_qty_milli: number | null;
    receive_unit_cost_minor: number | null;
    cost_exact: boolean;
  }[];
  supplier_invoice_id: string | null;
  receiving_draft_id: string | null;
}

export interface DocMetrics {
  documents: number;
  read: number;
  failed: number;
  lines: number;
  extraction_success_pct: number;
  ocr_failure_pct: number;
  supplier_auto_match_pct: number;
  product_auto_match_pct: number;
  correction_rate_pct: number;
  duplicates_detected: number;
  avg_processing_seconds: number;
}

export interface ReceivingDraftLine {
  line_no: number;
  product_id: string;
  product_name: string;
  description: string;
  qty_milli: number;
  unit_cost_minor: number;
  case_qty_milli: number | null;
  units_per_case: number | null;
  po_item_id: string | null;
  scan_line_no: number | null;
}

export interface ReceivingDraft {
  draft_id: string;
  number: string;
  status: string;
  supplier_id: string;
  supplier_name: string;
  po_id: string | null;
  po_number: string | null;
  scan_id: string | null;
  scan_number: string | null;
  reference: string | null;
  created_at: string;
  posted_at: string | null;
  posted_by_name: string | null;
  revision: number;
  total_minor: number;
  lines?: ReceivingDraftLine[];
}

export interface SupplierInvoiceLine {
  line_no: number;
  description: string;
  product_id: string | null;
  product_name: string | null;
  qty_milli: number | null;
  unit_cost_minor: number | null;
  vat_rate_bp: number | null;
  vat_minor: number | null;
  line_total_minor: number | null;
}

export interface SupplierInvoice {
  invoice_id: string;
  number: string;
  status: string;
  doc_type: string;
  invoice_number: string | null;
  invoice_date: string | null;
  supplier_name: string;
  total_minor: number | null;
  created_at: string;
  subtotal_minor?: number | null;
  vat_minor?: number | null;
  due_date?: string | null;
  posting?: string;
  posting_note?: string;
  scan_id?: string | null;
  scan_number?: string | null;
  approved_by_name?: string | null;
  lines?: SupplierInvoiceLine[];
}

// ---- WhatsApp AI orders (conversation → draft digital order → staff review)

export interface WaOrderRow {
  session_id: string;
  chat: string;
  phone: string | null;
  push_name: string | null;
  customer_id: string | null;
  customer_name: string | null;
  customer_state: string;
  order_id: string | null;
  order_number: string | null;
  order_status: string | null;
  payment_state: string | null;
  state: string;
  intent: string | null;
  priority: string;
  priority_reasons: string[] | null;
  open_questions: number;
  ai_status: string;
  staff_takeover: boolean;
  handled: boolean;
  last_message: string | null;
  last_message_at: string | null;
  delivery_mode: string;
  delivery_fee_minor: number | null;
  subtotal_minor?: number;
  total_minor?: number;
  complete?: boolean;
  updated_at: string;
}

export interface WaOrderOption {
  product_id: string;
  name: string;
  name_ar?: string | null;
  price_minor: number | null;
  availability: string;
  stock_milli?: number | null;
  score?: number;
  reasons?: string[];
}

export interface WaOrderQuestion {
  id: string;
  kind: string;
  line_no?: number | null;
  text: string;
  options: WaOrderOption[];
}

export interface WaOrderLine {
  line_no: number;
  product_id: string | null;
  name: string;
  requested: string | null;
  qty_milli: number;
  unit_price_minor: number | null;
  line_total_minor: number | null;
  resolution: string;
  availability: string;
  locked: boolean;
  note: string | null;
  candidates: WaOrderOption[] | null;
  alternatives?: WaOrderOption[];
}

export interface WaOrderDetail {
  session: {
    session_id: string;
    chat: string;
    phone: string | null;
    customer_id: string | null;
    customer_name: string | null;
    customer_state: string;
    customer_candidates: { customer_id: string; name: string }[] | null;
    order_id: string | null;
    state: string;
    intent: string | null;
    priority: string;
    priority_reasons: string[] | null;
    delivery_mode: string;
    address: { area: string | null; parts: AddressParts | null } | null;
    address_raw: string | null;
    address_source: string | null;
    zone_id: string | null;
    delivery_fee_minor: number | null;
    fee_state: string;
    ai_status: string;
    staff_takeover: boolean;
    handled: boolean;
    assigned_to: string | null;
    questions: WaOrderQuestion[];
    revision: number;
    created_at: string;
    updated_at: string;
  };
  messages: {
    seq: number;
    dir: "in" | "out";
    kind: string;
    text: string | null;
    at: string;
    intent?: string | null;
    status?: string | null;
  }[];
  order: {
    order_id: string;
    order_number: string;
    status: string;
    payment_state: string;
    lines: WaOrderLine[];
    subtotal_minor: number;
    delivery_fee_minor: number | null;
    total_minor: number;
    complete: boolean;
  } | null;
  payment_evidence?: {
    review_id: string;
    review_number: string;
    status: string;
    detected_minor: number | null;
    detected_reference: string | null;
    ocr_status: string | null;
    verified: boolean;
  }[];
  upsell?: { product_id: string; name: string; price_minor: number | null; reason: string }[];
  product_info?: { product_id: string; name: string; price_minor: number | null; availability: string }[];
  events: { seq: number | null; kind: string; source: string; user: string | null; at: string; data: unknown }[];
  suggested_reply: string;
  summary: string;
}

export interface WaOrderMetrics {
  messages_processed: number;
  order_intents: number;
  drafts: number;
  confirmed: number;
  conversion_pct: number;
  clarification_rate_pct: number;
  product_resolution_pct: number;
  staff_overrides: number;
  failed_jobs: number;
}

export interface DeliveryZone {
  zone_id: string;
  name: string;
  blocks: { from: number; to: number }[];
  areas: string[];
  fee_minor: number;
  free_over_minor: number | null;
  active: boolean;
}
