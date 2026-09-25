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
  "customers.credit": boolean;
  windows_hello: boolean;
  pdf_receipts: boolean;
  updates: boolean;
  "inventory.locations": boolean;
  "loyalty.enabled": boolean;
  "orders.digital": boolean;
  "org.multi_branch": boolean;
  "pwa.companion": boolean;
}

export type FeatureName = keyof FeatureFlags;

export interface PosConfig {
  pos: PosSettings;
  features: FeatureFlags;
  payments: TenderConfig[];
  shift: { blind_close: boolean };
  appearance: { theme: string; density: string; cashier_font: string };
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

export interface CustomerRef {
  customer_id: string;
  name: string;
  phone: string | null;
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
  kind: "receipt" | "dispatch" | "delivered" | "reminder" | "payment_ack" | "text" | "document";
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

export interface AiSettings {
  provider: "fake" | "anthropic" | "openai_compatible";
  model: string;
  base_url: string;
  max_tokens: number;
  fallbacks: boolean;
  consent: boolean;
  consent_by?: string | null;
  consent_at?: string | null;
}

export interface AiStatus {
  settings: AiSettings;
  key_configured: boolean;
  enabled: boolean;
  mutations: boolean;
  can_mutate: boolean;
  ready: boolean;
}

export interface AiProposal {
  proposal_id: string;
  proposal_number: string;
  conversation_id: string;
  kind: "price_change" | "stock_adjustment" | "purchase_order";
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
}

export interface AiConversation {
  conversation_id: string;
  title: string;
  untrusted_seen: boolean;
  messages: { role: "user" | "assistant"; text: string; tools: string[]; at: string; stop_reason: string | null }[];
  proposals: AiProposal[];
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
