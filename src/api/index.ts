// Typed API layer. Components never talk to the backend (or SQL) directly.
import { send } from "./transport";
import type * as T from "./types";

let token: string | null = null;
export const setToken = (t: string | null) => {
  token = t;
};
export const getToken = () => token;

const call = <R>(cmd: string, args: object = {}) => send<R>(cmd, token, args);

type Approval = { approval_token?: string | null };

export const api = {
  ping: () => call<{ ok: boolean; version: string }>("app.ping"),
  setup: {
    status: () => call<T.SetupStatus>("setup.status"),
    initialize: (req: Record<string, unknown>) => call<T.SetupStatus>("setup.initialize", req),
  },
  auth: {
    users: () => call<T.LoginUser[]>("auth.users"),
    login: (user_id: string, pin: string) =>
      call<{ token: string; session: T.Session }>("auth.login", { user_id, pin }),
    logout: () => call<void>("auth.logout"),
    lock: () => call<void>("auth.lock"),
    unlock: (pin: string) => call<T.Session>("auth.unlock", { pin }),
    session: () => call<T.Session>("auth.session"),
    touch: () => call<T.Session>("auth.touch"),
    approvers: (permission: string) => call<T.LoginUser[]>("auth.approvers", { permission }),
    approve: (approver_user_id: string, pin: string, permission: string, summary: string) =>
      call<{ approval_token: string; approver_name: string }>("auth.approve", {
        approver_user_id,
        pin,
        permission,
        summary,
      }),
    changePin: (current_pin: string, new_pin: string) => call<void>("auth.change_pin", { current_pin, new_pin }),
  },
  pos: {
    config: () => call<T.PosConfig>("pos.config"),
    cart: () => call<T.Cart>("pos.cart"),
    scan: (barcode: string, qty_milli?: number) => call<T.ScanResult>("pos.scan", { barcode, qty_milli }),
    search: (q: string, opts: { category_id?: string | null; favorites?: boolean; limit?: number } = {}) =>
      call<T.PosSearchRow[]>("pos.search", { q, ...opts }),
    addProduct: (product_id: string, qty_milli?: number) => call<T.Cart>("pos.add_product", { product_id, qty_milli }),
    addCustom: (a: { name: string; unit_price_minor: number; qty_milli?: number; tax_rule_id?: string } & Approval) =>
      call<T.Cart>("pos.add_custom", a),
    setQty: (line_id: string, qty_milli: number, approval_token?: string | null) =>
      call<T.Cart>("pos.set_qty", { line_id, qty_milli, approval_token }),
    removeLine: (line_id: string, approval_token?: string | null) =>
      call<T.Cart>("pos.remove_line", { line_id, approval_token }),
    lineDiscount: (line_id: string, discount_minor: number, discount_bp: number, approval_token?: string | null) =>
      call<T.Cart>("pos.line_discount", { line_id, discount_minor, discount_bp, approval_token }),
    cartDiscount: (discount_minor: number, discount_bp: number, approval_token?: string | null) =>
      call<T.Cart>("pos.cart_discount", { discount_minor, discount_bp, approval_token }),
    priceOverride: (line_id: string, unit_price_minor: number, reason: string | null, approval_token?: string | null) =>
      call<T.Cart>("pos.price_override", { line_id, unit_price_minor, reason, approval_token }),
    setCustomer: (customer_id: string | null) => call<T.Cart>("pos.set_customer", { customer_id }),
    hold: (note: string | null) => call<T.Cart>("pos.hold", { note }),
    held: () => call<T.HeldCart[]>("pos.held"),
    restore: (cart_id: string) => call<T.Cart>("pos.restore", { cart_id }),
    heldDelete: (cart_id: string, approval_token?: string | null) =>
      call<void>("pos.held_delete", { cart_id, approval_token }),
    cancel: (approval_token?: string | null) => call<T.Cart>("pos.cancel", { approval_token }),
    finalize: (
      a: { cart_id: string; operation_id: string; tenders: T.TenderInput[]; expected_total_minor?: number } & Approval,
    ) => call<T.SaleResult>("pos.finalize", a),
  },
  sales: {
    list: (q: Record<string, unknown>) => call<T.Page<T.SaleRow>>("sales.list", q),
    get: (sale_id: string) => call<T.SaleDetail>("sales.get", { sale_id }),
    findReceipt: (receipt_number: string) => call<T.SaleDetail>("sales.find_receipt", { receipt_number }),
    reprint: (sale_id: string) => call<T.PrintOutcome>("sales.reprint", { sale_id }),
  },
  refunds: {
    lookup: (receipt_number: string) => call<T.SaleDetail>("refunds.lookup", { receipt_number }),
    preview: (req: Record<string, unknown>) => call<T.RefundPreview>("refunds.preview", req),
    create: (req: Record<string, unknown>) => call<T.RefundResult>("refunds.create", req),
    list: (from?: string, to?: string) => call<Record<string, unknown>[]>("refunds.list", { from, to }),
  },
  receipts: {
    preview: (kind: "sale" | "refund" | "shift_report", ref_id: string) =>
      call<{ text: string; width_chars: number }>("receipts.preview", { kind, ref_id }),
  },
  print: {
    retry: (job_id: string) => call<T.PrintOutcome>("print.retry", { job_id }),
    queue: () => call<T.PrintJobRow[]>("print.queue"),
    test: () => call<T.PrintOutcome>("print.test"),
    drawerTest: () => call<T.PrintOutcome>("print.drawer_test"),
    printers: () => call<string[]>("print.printers"),
  },
  shift: {
    current: () => call<T.ShiftSummary | null>("shift.current"),
    open: (opening_float_minor: number, operation_id: string) =>
      call<T.ShiftSummary>("shift.open", { opening_float_minor, operation_id }),
    get: (shift_id: string) => call<T.ShiftSummary>("shift.get", { shift_id }),
    close: (
      a: { shift_id: string; counted_cash_minor: number; note?: string | null; operation_id: string } & Approval,
    ) => call<{ summary: T.ShiftSummary; print: T.PrintOutcome }>("shift.close", a),
    list: (from?: string, to?: string) => call<T.ShiftSummary[]>("shift.list", { from, to }),
  },
  cash: {
    event: (a: { kind: string; amount_minor: number; reason: string; operation_id: string } & Approval) =>
      call<Record<string, unknown>>("cash.event", a),
    list: (a: { shift_id?: string; from?: string; to?: string }) => call<Record<string, unknown>[]>("cash.list", a),
  },
  products: {
    search: (q: Record<string, unknown>) => call<T.Page<T.ProductRow>>("products.search", q),
    get: (product_id: string) => call<T.ProductDetail>("products.get", { product_id }),
    create: (
      a: T.ProductInput & {
        price_minor: number;
        cost_minor?: number | null;
        barcodes: string[];
        opening_stock_milli?: number | null;
      },
    ) => call<T.ProductDetail>("products.create", a),
    update: (a: T.ProductInput & { product_id: string; expected_version: number }) =>
      call<T.ProductDetail>("products.update", a),
    setActive: (product_id: string, active: boolean) =>
      call<T.ProductDetail>("products.set_active", { product_id, active }),
    bulkSetActive: (product_ids: string[], active: boolean) =>
      call<number>("products.bulk_set_active", { product_ids, active }),
    priceUpdate: (product_id: string, amount_minor: number, reason: string | null, effective_from?: string | null) =>
      call<T.ProductDetail>("products.price_update", { product_id, amount_minor, reason, effective_from }),
    bulkPrice: (changes: { product_id: string; amount_minor: number }[], reason: string, operation_id: string) =>
      call<{ changed: number }>("products.bulk_price", { changes, reason, operation_id }),
    costUpdate: (product_id: string, cost_minor: number, reason: string | null) =>
      call<T.ProductDetail>("products.cost_update", { product_id, cost_minor, reason }),
    exportCsv: (include_archived: boolean) => call<string>("products.export_csv", { include_archived }),
    importPreview: (a: Record<string, unknown>) => call<T.ImportPreview>("products.import_preview", a),
    importApply: (a: Record<string, unknown>) => call<Record<string, number>>("products.import_apply", a),
  },
  barcodes: {
    add: (product_id: string, barcode: string, make_primary = false) =>
      call<T.ProductDetail>("barcodes.add", { product_id, barcode, make_primary }),
    remove: (barcode_id: string) => call<T.ProductDetail>("barcodes.remove", { barcode_id }),
    setPrimary: (barcode_id: string) => call<T.ProductDetail>("barcodes.set_primary", { barcode_id }),
    unknown: (status?: string) => call<T.UnknownBarcodeRow[]>("barcodes.unknown_list", { status }),
    dismiss: (barcode: string) => call<void>("barcodes.unknown_dismiss", { barcode }),
  },
  categories: {
    list: (include_inactive = false) => call<T.CategoryRow[]>("categories.list", { include_inactive }),
    save: (a: { category_id?: string | null; name: string; parent_id?: string | null; sort_order?: number }) =>
      call<T.CategoryRow>("categories.save", a),
    archive: (category_id: string, reassign_to?: string | null) =>
      call<void>("categories.archive", { category_id, reassign_to }),
  },
  tax: {
    list: () => call<T.TaxRuleRow[]>("tax.list"),
    create: (name: string, rate_bp: number, inclusive: boolean, replace_rule_id?: string | null) =>
      call<T.TaxRuleRow[]>("tax.create", { name, rate_bp, inclusive, replace_rule_id }),
    setActive: (tax_rule_id: string, active: boolean) =>
      call<T.TaxRuleRow[]>("tax.set_active", { tax_rule_id, active }),
  },
  inventory: {
    movements: (q: Record<string, unknown>) => call<T.Page<T.MovementRow>>("inventory.movements", q),
    adjust: (a: { product_id: string; mode: string; qty_milli: number; reason: string; operation_id: string }) =>
      call<{ before_milli: number; after_milli: number }>("inventory.adjust", a),
    receive: (a: Record<string, unknown>) => call<Record<string, unknown>>("inventory.receive", a),
  },
  stocktake: {
    list: () => call<T.StocktakeRow[]>("stocktake.list"),
    create: (a: Record<string, unknown>) => call<T.StocktakeDetail>("stocktake.create", a),
    get: (stocktake_id: string) => call<T.StocktakeDetail>("stocktake.get", { stocktake_id }),
    count: (a: {
      stocktake_id: string;
      product_id?: string;
      barcode?: string;
      qty_milli: number;
      mode: "set" | "add";
    }) => call<T.StocktakeLine>("stocktake.count", a),
    setStatus: (stocktake_id: string, status: string) =>
      call<T.StocktakeDetail>("stocktake.set_status", { stocktake_id, status }),
    finalize: (stocktake_id: string, operation_id: string) =>
      call<Record<string, unknown>>("stocktake.finalize", { stocktake_id, operation_id }),
  },
  suppliers: {
    list: (q?: string, include_inactive = false) => call<T.SupplierRow[]>("suppliers.list", { q, include_inactive }),
    get: (supplier_id: string) =>
      call<{ supplier: T.SupplierRow; purchase_orders: T.PoRow[]; products: Record<string, unknown>[] }>(
        "suppliers.get",
        { supplier_id },
      ),
    save: (supplier_id: string | null, supplier: T.SupplierInput) =>
      call<T.SupplierRow>("suppliers.save", { supplier_id, supplier }),
  },
  po: {
    list: (status?: string, supplier_id?: string) => call<T.PoRow[]>("po.list", { status, supplier_id }),
    get: (po_id: string) => call<T.PoDetail>("po.get", { po_id }),
    save: (po_id: string | null, po: Record<string, unknown>) => call<T.PoDetail>("po.save", { po_id, po }),
    setStatus: (po_id: string, status: string) => call<T.PoDetail>("po.set_status", { po_id, status }),
    receive: (a: {
      po_id: string;
      reference?: string | null;
      lines: { po_item_id: string; qty_milli: number; unit_cost_minor?: number | null }[];
      operation_id: string;
    }) => call<T.PoDetail>("po.receive", a),
  },
  customers: {
    search: (q?: string, include_inactive = false, limit?: number) =>
      call<T.CustomerRow[]>("customers.search", { q, include_inactive, limit }),
    get: (customer_id: string) =>
      call<{
        customer: T.CustomerRow;
        notes: Record<string, string>[];
        purchases: Record<string, unknown>[];
        deliveries: T.DeliveryRow[];
      }>("customers.get", { customer_id }),
    save: (customer_id: string | null, customer: T.CustomerInput) =>
      call<T.CustomerRow>("customers.save", { customer_id, customer }),
    addNote: (customer_id: string, note: string) => call<void>("customers.add_note", { customer_id, note }),
  },
  deliveries: {
    list: (status?: string, include_closed = false) =>
      call<T.DeliveryRow[]>("deliveries.list", { status, include_closed }),
    get: (delivery_id: string) =>
      call<{ delivery: T.DeliveryRow; events: Record<string, string>[]; items: Record<string, unknown>[] }>(
        "deliveries.get",
        { delivery_id },
      ),
    create: (a: Record<string, unknown>) => call<T.DeliveryRow>("deliveries.create", a),
    update: (a: {
      delivery_id: string;
      status?: string;
      assigned_user_id?: string;
      payment_status?: string;
      note?: string;
    }) => call<T.DeliveryRow>("deliveries.update", a),
  },
  reports: {
    catalog: () => call<{ key: string; title: string; group: string; description: string }[]>("reports.catalog"),
    run: (key: string, params: T.ReportParams) => call<T.Report>("reports.run", { key, params }),
    csv: (key: string, params: T.ReportParams) => call<string>("reports.csv", { key, params }),
    dashboard: () => call<Record<string, unknown>>("dashboard.get"),
  },
  users: {
    list: () => call<T.UserRow[]>("users.list"),
    create: (user: { display_name: string; role_id: string; pin: string; active: boolean }) =>
      call<T.UserRow>("users.create", { user }),
    update: (user_id: string, user: { display_name: string; role_id: string; pin?: string | null; active: boolean }) =>
      call<T.UserRow>("users.update", { user_id, user }),
    unlock: (user_id: string) => call<T.UserRow>("users.unlock", { user_id }),
  },
  roles: {
    list: () => call<T.RoleRow[]>("roles.list"),
    permissions: () => call<T.PermissionRow[]>("roles.permissions"),
    save: (role_id: string | null, name: string, description: string | null, permissions: string[]) =>
      call<T.RoleRow[]>("roles.save", { role_id, name, description, permissions }),
  },
  settings: {
    get: <V = Record<string, unknown>>(key: string) => call<V>("settings.get", { key }),
    save: <V = Record<string, unknown>>(key: string, value: V) => call<V>("settings.save", { key, value }),
  },
  business: {
    get: () => call<Record<string, unknown>>("business.get"),
    update: (p: Record<string, unknown>) => call<Record<string, unknown>>("business.update", p),
  },
  audit: {
    list: (q: Record<string, unknown>) => call<T.Page<T.AuditRow>>("audit.list", q),
    verify: () =>
      call<{ entries: number; valid: boolean; first_broken_seq: number | null; message: string }>("audit.verify"),
  },
  devices: {
    list: () => call<T.DeviceRow[]>("devices.list"),
    rename: (device_id: string, name: string) => call<T.DeviceRow[]>("devices.rename", { device_id, name }),
    setActive: (device_id: string, active: boolean) => call<T.DeviceRow[]>("devices.set_active", { device_id, active }),
  },
  diagnostics: {
    get: (full = false) => call<T.DiagnosticItem[]>("diagnostics.get", { full }),
    export: () => call<Record<string, unknown>>("diagnostics.export"),
  },
  backup: {
    list: () => call<Record<string, unknown> & { backups: T.BackupRow[] }>("backup.list"),
    health: () => call<T.DiagnosticItem>("backup.health"),
    create: (directory?: string | null) => call<T.BackupRow>("backup.create", { directory }),
    inspect: (path: string) => call<T.BackupInspection>("backup.inspect", { path }),
    restore: (path: string, acknowledge_different_business = false) =>
      call<Record<string, unknown>>("backup.restore", { path, acknowledge_different_business }),
  },
  sync: {
    status: () => call<Record<string, unknown>>("sync.status"),
    enableHub: () => call<Record<string, unknown>>("sync.enable_hub"),
    pairingCode: (device_name?: string) =>
      call<{ code: string; expires_at: string }>("sync.pairing_code", { device_name }),
    hubAddresses: () => call<{ addresses: string[]; port: number; running: boolean }>("sync.hub_addresses"),
    discover: () => call<{ url: string; hub_name: string; business_name: string; version: string }[]>("sync.discover"),
    probe: (hub_url: string) => call<{ url: string; info: Record<string, unknown> }>("sync.probe", { hub_url }),
    join: (a: { hub_url: string; code: string; device_name: string; device_code: string }) =>
      call<T.SetupStatus>("sync.join", a),
    runNow: () => call<Record<string, number>>("sync.run_now"),
    resetHubCredentials: () => call<Record<string, unknown>>("sync.reset_hub_credentials"),
    deadLetters: () => call<Record<string, unknown>[]>("sync.dead_letters"),
    retryDeadLetter: (dead_id: string) => call<Record<string, unknown>>("sync.retry_dead_letter", { dead_id }),
    unblock: (accept_new_hub: boolean) => call<Record<string, unknown>>("sync.unblock", { accept_new_hub }),
  },
};

export type Api = typeof api;
export { ApiError } from "./transport";
