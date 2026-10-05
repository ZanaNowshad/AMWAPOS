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
    approve: (approver_user_id: string, pin: string, permission: string, summary: string, binding?: string | null) =>
      call<{ approval_token: string; approver_name: string }>("auth.approve", {
        approver_user_id,
        pin,
        permission,
        summary,
        binding: binding ?? null,
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
    loyaltyRedeem: (points: number) => call<T.Cart>("pos.loyalty_redeem", { points }),
    finalize: (
      a: {
        cart_id: string;
        operation_id: string;
        tenders: T.TenderInput[];
        expected_total_minor?: number;
        fulfilment?: T.Fulfilment | null;
      } & Approval,
    ) => call<T.SaleResult>("pos.finalize", a),
  },
  sales: {
    list: (q: Record<string, unknown>) => call<T.Page<T.SaleRow>>("sales.list", q),
    get: (sale_id: string) => call<T.SaleDetail>("sales.get", { sale_id }),
    findReceipt: (receipt_number: string) => call<T.SaleDetail>("sales.find_receipt", { receipt_number }),
    reprint: (sale_id: string) => call<T.PrintOutcome>("sales.reprint", { sale_id }),
    voidCheck: (sale_id: string) => call<T.VoidCheck>("sales.void_check", { sale_id }),
    void: (sale_id: string, reason: string, operation_id: string, approval_token: string | null) =>
      call<T.VoidResult>("sales.void", { void: { sale_id, reason, operation_id, approval_token } }),
  },
  statements: {
    get: (customer_id: string, from?: string | null, to?: string | null) =>
      call<T.CustomerStatement>("customers.statement", { customer_id, from: from ?? null, to: to ?? null }),
    pdf: (customer_id: string, from?: string | null, to?: string | null) =>
      call<{ file_name: string; base64: string; text: string }>("customers.statement_pdf", {
        customer_id,
        from: from ?? null,
        to: to ?? null,
      }),
    receivables: (as_of?: string | null) => call<T.Receivables>("customers.receivables", { as_of: as_of ?? null }),
    setTerms: (customer_id: string, terms_days: number) =>
      call<{ terms_days: number }>("customers.terms_set", { customer_id, terms_days }),
  },
  expenses: {
    list: (from?: string | null, to?: string | null, status?: string | null) =>
      call<T.ExpenseList>("expenses.list", { from: from ?? null, to: to ?? null, status: status ?? null }),
    get: (expense_id: string) =>
      call<{ expense: T.Expense; attachments: T.ExpenseAttachment[] }>("expenses.get", { expense_id }),
    save: (expense_id: string | null, expense: T.ExpenseInput) =>
      call<T.Expense>("expenses.save", { expense_id, expense }),
    deleteDraft: (expense_id: string) => call<null>("expenses.delete_draft", { expense_id }),
    submit: (expense_id: string) => call<T.Expense>("expenses.submit", { expense_id }),
    decide: (expense_id: string, approve: boolean, note: string | null) =>
      call<T.Expense>("expenses.decide", { expense_id, approve, note }),
    pay: (
      expense_id: string,
      payment: {
        method: string;
        fund_id?: string | null;
        cash_event_id?: string | null;
        reference?: string | null;
        operation_id: string;
      },
    ) => call<T.Expense>("expenses.pay", { expense_id, payment }),
    void: (expense_id: string, reason: string, operation_id: string) =>
      call<T.Expense>("expenses.void", { expense_id, reason, operation_id }),
    attach: (expense_id: string, file_name: string, data: string) =>
      call<{ attachment_id: string }>("expenses.attach", { expense_id, file_name, data }),
    attachment: (attachment_id: string) =>
      call<{ file_name: string; mime: string; base64: string }>("expenses.attachment", { attachment_id }),
    categories: () => call<T.ExpenseCategory[]>("expenses.categories", {}),
    categorySave: (category_id: string | null, name: string, name_ar: string | null, active: boolean) =>
      call<{ category_id: string }>("expenses.category_save", { category_id, name, name_ar, active }),
    recurring: () => call<T.ExpenseRecurring[]>("expenses.recurring", {}),
    recurringSave: (recurring_id: string | null, recurring: T.ExpenseRecurringInput) =>
      call<{ recurring_id: string; next_date: string }>("expenses.recurring_save", { recurring_id, recurring }),
    unlinkedPaidOuts: () => call<T.TillPaidOut[]>("expenses.unlinked_paid_outs", {}),
  },
  petty: {
    funds: () => call<T.PettyFund[]>("petty.funds", {}),
    fundSave: (fund_id: string | null, name: string, custodian_user_id: string | null, active: boolean) =>
      call<{ fund_id: string }>("petty.fund_save", { fund_id, name, custodian_user_id, active }),
    entry: (fund_id: string, kind: string, amount_minor: number, note: string | null, operation_id: string) =>
      call<{ balance_minor: number }>("petty.entry", { fund_id, kind, amount_minor, note, operation_id }),
    count: (fund_id: string, counted_minor: number, note: string | null, operation_id: string) =>
      call<{ expected_minor: number; counted_minor: number; difference_minor: number }>("petty.count", {
        fund_id,
        counted_minor,
        note,
        operation_id,
      }),
    entries: (fund_id: string) => call<T.PettyEntry[]>("petty.entries", { fund_id }),
  },
  refunds: {
    lookup: (receipt_number: string) => call<T.SaleDetail>("refunds.lookup", { receipt_number }),
    preview: (req: Record<string, unknown>) => call<T.RefundPreview>("refunds.preview", req),
    create: (req: Record<string, unknown>) => call<T.RefundResult>("refunds.create", req),
    list: (from?: string, to?: string) => call<Record<string, unknown>[]>("refunds.list", { from, to }),
  },
  receipts: {
    pdf: (kind: "sale" | "refund", ref_id: string) =>
      call<{ file_name: string; path: string; base64: string }>("receipts.pdf", { kind, ref_id }),
    preview: (kind: "sale" | "refund" | "shift_report", ref_id: string) =>
      call<{ text: string; width_chars: number; sha256?: string | null; exact?: boolean | null }>("receipts.preview", {
        kind,
        ref_id,
      }),
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
        image_b64?: string | null;
      },
    ) => call<T.ProductDetail>("products.create", a),
    /** Stored images by hash, as data URLs (unknown hashes are left out). */
    images: (hashes: string[]) => call<Record<string, string>>("products.images", { hashes }),
    imageState: (product_id: string) => call<T.ProductImageState>("products.image_state", { product_id }),
    imageUpload: (product_id: string, data: string) =>
      call<T.ProductImageState>("products.image_upload", { product_id, data }),
    imageRemove: (product_id: string) => call<T.ProductImageState>("products.image_remove", { product_id }),
    imageFind: (product_id: string) => call<T.ProductImageState>("products.image_find", { product_id }),
    imageBackfill: (limit?: number) => call<{ queued: number }>("products.image_backfill", { limit }),
    imageOverview: () => call<T.ImageOverview>("products.image_overview"),
    imageConfigure: (settings: T.ImageSearchSettings, google_key?: string | null) =>
      call<T.ImageOverview>("products.image_configure", { settings, google_key }),
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
    reopen: (barcode: string) => call<void>("barcodes.unknown_reopen", { barcode }),
    merge: (product_id: string, barcodes: string[]) =>
      call<{ product_id: string; product_name: string; added: number; resolved: number }>("barcodes.unknown_merge", {
        product_id,
        barcodes,
      }),
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
    blockArea: (block: string) => call<string | null>("customers.block_area", { block }),
    save: (customer_id: string | null, customer: T.CustomerInput) =>
      call<T.CustomerRow>("customers.save", { customer_id, customer }),
    addNote: (customer_id: string, note: string) => call<void>("customers.add_note", { customer_id, note }),
    account: (customer_id: string) => call<T.CustomerAccountView>("customers.account", { customer_id }),
    accountSet: (customer_id: string, enabled: boolean, credit_limit_minor: number) =>
      call<T.CustomerAccountView>("customers.account_set", { customer_id, enabled, credit_limit_minor }),
    accountPayment: (a: {
      customer_id: string;
      amount_minor: number;
      method: string;
      reference?: string | null;
      operation_id: string;
    }) => call<{ balance_minor: number }>("customers.account_payment", a),
    accountAdjust: (customer_id: string, amount_minor: number, note: string, operation_id: string) =>
      call<{ balance_minor: number }>("customers.account_adjust", { customer_id, amount_minor, note, operation_id }),
    addressSave: (a: {
      address_id?: string | null;
      customer_id: string;
      label: string;
      area?: string | null;
      address: string;
      notes?: string | null;
      is_default: boolean;
    }) => call<T.CustomerAccountView>("customers.address_save", a),
    addressDelete: (address_id: string) => call<void>("customers.address_delete", { address_id }),
  },
  tickets: {
    list: (f: T.TicketFilter = {}) => call<T.TicketRow[]>("tickets.list", f),
    counts: () => call<T.TicketCounts>("tickets.counts"),
    get: (ticket_id: string) => call<T.TicketSheet>("tickets.get", { ticket_id }),
    recordPayment: (a: {
      delivery_id: string;
      method: string;
      amount_minor?: number | null;
      reference?: string | null;
      operation_id: string;
    }) => call<T.TicketRow>("tickets.record_payment", a),
    unable: (delivery_id: string, reason: string) => call<T.TicketRow>("tickets.unable", { delivery_id, reason }),
    notDelivered: (a: {
      delivery_id: string;
      reason: string;
      restock: boolean;
      refund_method?: string | null;
      operation_id: string;
      approval_token?: string | null;
    }) => call<T.TicketRow>("tickets.not_delivered", a),
  },
  riders: {
    cash: () => call<T.RiderCash[]>("riders.cash"),
    handover: (a: {
      rider_user_id: string;
      collect: string[];
      counted_minor: number;
      note?: string | null;
      operation_id: string;
    }) => call<T.RiderHandover>("riders.handover", a),
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
    revert: (delivery_id: string, status: string) =>
      call<T.DeliveryRow>("deliveries.update", { delivery_id, status, revert: true }),
  },
  reports: {
    catalog: () => call<{ key: string; title: string; group: string; description: string }[]>("reports.catalog"),
    run: (key: string, params: T.ReportParams) => call<T.Report>("reports.run", { key, params }),
    csv: (key: string, params: T.ReportParams) => call<string>("reports.csv", { key, params }),
    dashboard: () => call<Record<string, unknown>>("dashboard.get"),
    eod: (date?: string | null, branch_id?: string | null) => call<T.EodPack>("reports.eod", { date, branch_id }),
    eodZip: (date?: string | null, branch_id?: string | null) =>
      call<{ file_name: string; base64: string; files: string[] }>("reports.eod_zip", { date, branch_id }),
    presets: () => call<T.ReportPreset[]>("reports.presets"),
    presetSave: (preset: Omit<T.ReportPreset, "preset_id">) =>
      call<T.ReportPreset[]>("reports.preset_save", { preset }),
    presetDelete: (preset_id: string) => call<T.ReportPreset[]>("reports.preset_delete", { preset_id }),
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
    pairingCode: (device_name?: string, branch_id?: string | null) =>
      call<{ code: string; expires_at: string }>("sync.pairing_code", { device_name, branch_id }),
    hubAddresses: () =>
      call<{ addresses: string[]; port: number; running: boolean; ips: string[]; bind_address: string }>(
        "sync.hub_addresses",
      ),
    setBindAddress: (address: string) =>
      call<{ bind_address: string; port: number }>("sync.set_bind_address", { address }),
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
  whatsapp: {
    /** WhatsApp + OCR status (separate flags). */
    status: () => call<T.AutomationStatus>("whatsapp.status"),
    start: () => call<T.WaStatus>("whatsapp.start"),
    pairCode: (phone: string) => call<T.WaStatus>("whatsapp.pair_code", { phone }),
    stop: () => call<T.WaStatus>("whatsapp.stop"),
    logout: () => call<T.WaStatus>("whatsapp.logout"),
    recent: (limit?: number) => call<T.WaRecent>("whatsapp.recent", { limit }),
    sessionBackup: (acknowledge_risk: boolean) =>
      call<{ path: string }>("whatsapp.session_backup", { acknowledge_risk }),
    queue: (req: T.WaQueueRequest) => call<T.WaOutboxRow>("whatsapp.queue", req),
    outbox: (status?: string) => call<T.WaOutboxRow[]>("whatsapp.outbox", { status }),
    outboxAction: (message_id: string, action: "retry" | "cancel") =>
      call<T.WaOutboxRow>("whatsapp.outbox_action", { message_id, action }),
    conversations: () => call<T.WaConversation[]>("whatsapp.conversations"),
    thread: (chat: string) => call<T.WaThread>("whatsapp.thread", { chat }),
    media: (seq: number) => call<T.FileBlob>("whatsapp.media", { seq }),
    markRead: (chat: string) => call<number>("whatsapp.mark_read", { chat }),
    summary: () => call<{ unread: number; queued: number; failed: number }>("whatsapp.summary"),
    phoneContacts: () => call<T.WaPhoneContacts>("whatsapp.phone_contacts"),
    threadContext: (chat: string) => call<T.WaThreadContext>("whatsapp.thread_context", { chat }),
    linkCustomer: (chat: string, customer_id: string | null) =>
      call<T.WaThreadContext>("whatsapp.link_customer", { chat, customer_id }),
    phoneContactsRefresh: () => call<{ requested: boolean }>("whatsapp.phone_contacts_refresh"),
    /** WhatsApp Business catalogue (same linked session). */
    catalogStatus: () => call<T.WaCatalogStatus>("whatsapp.catalog_status"),
    catalogSync: () => call<T.WaCatalogOverview>("whatsapp.catalog_sync"),
    catalogRetry: (product_id?: string) => call<{ queued: number }>("whatsapp.catalog_retry", { product_id }),
    catalogConfigure: (auto_sync: boolean) => call<{ auto_sync: boolean }>("whatsapp.catalog_configure", { auto_sync }),
    catalogRecheck: () => call<{ requested: boolean }>("whatsapp.catalog_recheck"),
    catalogProduct: (product_id: string) => call<T.WaCatalogProductState>("whatsapp.catalog_product", { product_id }),
    phoneContactsImport: (jids: string[] | null, update_existing: boolean) =>
      call<{ created: number; updated: number; skipped: number }>("whatsapp.phone_contacts_import", {
        jids,
        update_existing,
      }),
    importContacts: (chats?: string[]) =>
      call<{ created: number; linked: number; skipped: number }>("whatsapp.import_contacts", { chats }),
    triage: (limit?: number) =>
      call<{ items: T.WaTriageItem[]; counts: Record<string, number> }>("whatsapp.triage", { limit }),
    triageSet: (seq: number, category: T.WaTriageItem["category"]) =>
      call<{ seq: number; category: string }>("whatsapp.triage_set", { seq, category }),
    triageAi: (limit?: number) => call<{ updated: number }>("whatsapp.triage_ai", { limit }),
    /** A draft for a person to edit and send; it is never queued by itself. */
    draftReply: (chat: string, instruction?: string | null) =>
      call<{ chat: string; text: string; source: "ai" | "template"; category: string; lang: "en" | "ar" }>(
        "whatsapp.draft_reply",
        { chat, instruction },
      ),
  },
  payreviews: {
    list: (status?: string) => call<T.PaymentReview[]>("payreviews.list", { status }),
    get: (review_id: string) =>
      call<{ review: T.PaymentReview; image: T.FileBlob | null }>("payreviews.get", { review_id }),
    upload: (a: { file_name: string; data: string; expected_minor?: number | null; delivery_id?: string | null }) =>
      call<T.PaymentReview>("payreviews.upload", a),
    setExpected: (review_id: string, expected_minor: number | null, delivery_id?: string | null) =>
      call<T.PaymentReview>("payreviews.set_expected", { review_id, expected_minor, delivery_id }),
    decide: (a: {
      review_id: string;
      decision: "confirm" | "reject";
      note?: string | null;
      delivery_id?: string | null;
    }) => call<T.PaymentReview>("payreviews.decide", a),
  },
  invoiceScan: {
    import: (a: { file_name: string; data: string; supplier_id?: string | null }) =>
      call<T.InvoiceScan>("invoicescan.import", a),
    list: (status?: string) => call<T.InvoiceScan[]>("invoicescan.list", { status }),
    get: (scan_id: string) => call<T.InvoiceScanDetail>("invoicescan.get", { scan_id }),
    updateLine: (a: {
      scan_id: string;
      line_no: number;
      product_id?: string | null;
      clear_product?: boolean;
      qty_milli?: number | null;
      unit_cost_minor?: number | null;
      include?: boolean | null;
    }) => call<T.InvoiceScanDetail>("invoicescan.update_line", a),
    confirm: (scan_id: string, supplier_id: string, receive = false) =>
      call<T.InvoiceScanDetail>("invoicescan.confirm", { scan_id, supplier_id, receive }),
    reject: (scan_id: string, reason: string) => call<T.InvoiceScan>("invoicescan.reject", { scan_id, reason }),
    /** Ask the AI provider to re-read the lines ("Improve parse"); the review still applies. */
    aiParse: (scan_id: string) => call<{ scan_id: string; replaced: boolean }>("invoicescan.ai_parse", { scan_id }),
  },
  docs: {
    import: (a: { file_name: string; data: string; supplier_id?: string | null }) =>
      call<T.InvoiceScan>("docs.import", a),
    fromInbox: (seq: number) => call<T.InvoiceScan>("docs.from_inbox", { seq }),
    get: (scan_id: string) => call<T.DocDetail>("docs.get", { scan_id }),
    page: (scan_id: string, page: number) =>
      call<{ page: number; image: T.FileBlob | null }>("docs.page", { scan_id, page }),
    update: (
      scan_id: string,
      patch: {
        revision: number;
        doc_type?: string;
        supplier_id?: string;
        invoice_number?: string;
        invoice_date?: string;
        due_date?: string;
        subtotal_minor?: number;
        vat_minor?: number;
        total_minor?: number;
        vat_rate_bp?: number;
        po_id?: string;
      },
    ) => call<T.DocDetail>("docs.update", { scan_id, ...patch }),
    updateLine: (
      scan_id: string,
      patch: {
        revision: number;
        line_no: number;
        product_id?: string;
        clear_product?: boolean;
        qty_milli?: number;
        unit_cost_minor?: number;
        units_per_case?: number;
        unit?: string;
        include?: boolean;
        new_product?: boolean;
        not_item?: boolean;
      },
    ) => call<T.DocDetail>("docs.update_line", { scan_id, ...patch }),
    newProduct: (scan_id: string, line_no: number) =>
      call<Record<string, unknown>>("docs.new_product", { scan_id, line_no }),
    createSupplierInvoice: (scan_id: string, revision: number) =>
      call<T.SupplierInvoice>("docs.create_supplier_invoice", { scan_id, revision }),
    createReceiving: (scan_id: string, revision: number) =>
      call<T.ReceivingDraft>("docs.create_receiving", { scan_id, revision }),
    metrics: () => call<T.DocMetrics>("docs.metrics"),
  },
  receivingDrafts: {
    list: (status?: string | null) => call<T.ReceivingDraft[]>("receiving.drafts", { status }),
    get: (draft_id: string) => call<T.ReceivingDraft>("receiving.draft_get", { draft_id }),
    updateLine: (
      draft_id: string,
      line_no: number,
      a: { qty_milli?: number; unit_cost_minor?: number; remove?: boolean },
    ) => call<T.ReceivingDraft>("receiving.draft_update_line", { draft_id, line_no, ...a }),
    cancel: (draft_id: string) => call<T.ReceivingDraft>("receiving.draft_cancel", { draft_id }),
    /** Posts stock through the normal receiving commands (a person's action). */
    post: (draft_id: string, operation_id: string) =>
      call<T.ReceivingDraft>("receiving.draft_post", { draft_id, operation_id }),
  },
  payables: {
    overview: () => call<T.ApOverview>("ap.overview"),
    supplier: (supplier_id: string) => call<T.ApSupplier>("ap.supplier", { supplier_id }),
    invoice: (invoice_id: string) => call<T.ApInvoice>("ap.invoice_get", { invoice_id }),
    createInvoice: (invoice: T.ApInvoiceInput) => call<T.ApInvoice>("ap.invoice_create", { invoice }),
    approve: (invoice_id: string) => call<T.SupplierInvoice>("ap.invoice_approve", { invoice_id }),
    post: (invoice_id: string, operation_id: string) =>
      call<T.ApInvoice>("ap.invoice_post", { invoice_id, operation_id }),
    reverse: (invoice_id: string, reason: string) => call<T.ApInvoice>("ap.invoice_reverse", { invoice_id, reason }),
    recordPayment: (payment: T.ApPaymentInput) => call<T.ApPayment>("ap.payment_record", { payment }),
    reversePayment: (payment_id: string, reason: string) =>
      call<T.ApPayment>("ap.payment_reverse", { payment_id, reason }),
  },
  supplierInvoices: {
    list: (status?: string | null) => call<T.SupplierInvoice[]>("supplier_invoices.list", { status }),
    get: (invoice_id: string) => call<T.SupplierInvoice>("supplier_invoices.get", { invoice_id }),
    setStatus: (invoice_id: string, status: "approved" | "void") =>
      call<T.SupplierInvoice>("supplier_invoices.set_status", { invoice_id, status }),
  },
  waOrders: {
    list: (filter: "open" | "attention" | "all" = "open") => call<T.WaOrderRow[]>("waorders.list", { filter }),
    get: (session_id: string) => call<T.WaOrderDetail>("waorders.get", { session_id }),
    line: (
      session_id: string,
      revision: number,
      a: {
        line_no?: number | null;
        product_id?: string | null;
        qty_milli?: number | null;
        remove?: boolean;
        learn?: boolean;
      },
    ) => call<T.WaOrderDetail>("waorders.line", { session_id, revision, ...a }),
    delivery: (
      session_id: string,
      revision: number,
      mode: "delivery" | "pickup" | "unknown",
      address_parts?: T.AddressParts | null,
      zone_id?: string | null,
    ) => call<T.WaOrderDetail>("waorders.delivery", { session_id, revision, mode, address_parts, zone_id }),
    customer: (session_id: string, revision: number, customer_id: string) =>
      call<T.WaOrderDetail>("waorders.customer", { session_id, revision, customer_id }),
    flags: (session_id: string, a: { takeover?: boolean; handled?: boolean; assign_to_me?: boolean }) =>
      call<T.WaOrderDetail>("waorders.flags", { session_id, ...a }),
    confirm: (session_id: string, revision: number, ack: T.ConfirmAck = {}) =>
      call<T.WaOrderDetail>("waorders.confirm", { session_id, revision, ...ack }),
    cancel: (session_id: string) => call<T.WaOrderDetail>("waorders.cancel", { session_id }),
    payment: (session_id: string, review_id: string, decision: "verified" | "rejected", note?: string | null) =>
      call<T.WaOrderDetail>("waorders.payment", { session_id, review_id, decision, note }),
    send: (session_id: string, text?: string | null) => call<unknown>("waorders.send", { session_id, text }),
    metrics: () => call<T.WaOrderMetrics>("waorders.metrics"),
  },
  updates: {
    status: () => call<T.UpdateStatus>("updates.status"),
    check: () => call<{ newer: boolean; manifest: T.UpdateManifest; status: T.UpdateStatus }>("updates.check"),
    download: () => call<T.UpdateStatus>("updates.download"),
    install: () => call<{ started: boolean; version: string }>("updates.install"),
  },
  migration: {
    read: (files: { name: string; data: string }[]) =>
      call<{ tables: T.MigrationTable[]; ignored: string[]; fields: Record<string, { key: string; label: string }[]> }>(
        "migration.read",
        {
          files,
        },
      ),
    preview: (a: { table: T.MigrationTable; update_existing: boolean; skip_errors: boolean }) =>
      call<{ entity: string; preview: Record<string, unknown> }>("migration.preview", a),
    apply: (a: { table: T.MigrationTable; update_existing: boolean; skip_errors: boolean; operation_id: string }) =>
      call<{ entity: string; result: Record<string, unknown> }>("migration.apply", a),
  },
  ai: {
    status: () => call<T.AiStatus>("ai.status"),
    /** Owner only. Keys go to Windows Credential Manager; null keeps, "" removes. */
    configure: (
      settings: T.AiSettings,
      api_key?: string | null,
      extra_header_value?: string | null,
      fallback_api_key?: string | null,
    ) => call<T.AiStatus>("ai.configure", { settings, api_key, extra_header_value, fallback_api_key }),
    test: () => call<T.AiTestResult>("ai.test"),
    models: () => call<{ models: string[] }>("ai.models"),
    ask: (
      message: string,
      conversation_id?: string | null,
      locale?: string,
      extra?: { stream_id?: string; images?: string[]; context?: T.AiContext | null },
    ) => call<T.AiConversation>("ai.ask", { message, conversation_id, locale, ...extra }),
    /** Live events of a question started with stream_id (poll while it runs). */
    stream: (stream_id: string, after: number) =>
      call<{ events: T.AiStreamEvent[]; next: number; done: boolean; known: boolean }>("ai.stream", {
        stream_id,
        after,
      }),
    attachImage: (media_type: string, data: string) =>
      call<{ attachment_id: string; media_type: string; bytes: number }>("ai.attach_image", { media_type, data }),
    attachment: (attachment_id: string) =>
      call<{ attachment_id: string; media_type: string; data: string }>("ai.attachment", { attachment_id }),
    rename: (conversation_id: string, title: string) =>
      call<{ conversation_id: string; title: string }>("ai.conversation_rename", { conversation_id, title }),
    pin: (conversation_id: string, kind: T.AiPin["kind"], id: string) =>
      call<{ pins: T.AiPin[] }>("ai.pin", { conversation_id, kind, id }),
    unpin: (conversation_id: string, kind: T.AiPin["kind"], id: string) =>
      call<{ pins: T.AiPin[] }>("ai.unpin", { conversation_id, kind, id }),
    briefings: () => call<T.AiBriefing[]>("ai.briefings"),
    briefingSave: (
      briefing_id: string | null,
      briefing: Pick<T.AiBriefing, "name" | "playbook" | "at_time" | "days" | "with_ai" | "enabled">,
    ) => call<T.AiBriefing[]>("ai.briefing_save", { briefing_id, briefing }),
    briefingDelete: (briefing_id: string) => call<T.AiBriefing[]>("ai.briefing_delete", { briefing_id }),
    briefingRun: (briefing_id: string) => call<{ note_id: string; status: string }>("ai.briefing_run", { briefing_id }),
    notes: (limit?: number) => call<T.AiNote[]>("ai.notes", { limit }),
    alerts: (include_dismissed = false) => call<T.AiAlert[]>("ai.alerts", { include_dismissed }),
    alertDismiss: (alert_id: string) => call<T.AiAlert[]>("ai.alert_dismiss", { alert_id }),
    noteRead: (note_id: string) => call<void>("ai.note_read", { note_id }),
    /** A slash command that reads (no model involved). */
    slash: (command: string, arg?: string) => call<T.AiSlashResult>("ai.slash", { command, arg }),
    conversations: () =>
      call<{ conversation_id: string; title: string; updated_at: string; open_proposals: number }[]>(
        "ai.conversations",
      ),
    conversation: (conversation_id: string) => call<T.AiConversation>("ai.conversation", { conversation_id }),
    proposals: (status?: string) => call<T.AiProposal[]>("ai.proposals", { status }),
    /** Runs the proposal's command with your session; the card supplies approval_token and PIN inputs. */
    confirm: (proposal_id: string, approval_token?: string | null, inputs?: Record<string, string>) =>
      call<T.AiConfirmResult>("ai.proposal_confirm", { proposal_id, approval_token, inputs }),
    digest: (date?: string) => call<T.AiDigest>("ai.digest", { date }),
    playbook: (name: "eod" | "cash_short" | "reorder" | "refund_spike") =>
      call<T.AiPlaybookResult>("ai.playbook", { name }),
    reject: (proposal_id: string) => call<T.AiProposal>("ai.proposal_reject", { proposal_id }),
    undo: (proposal_id: string) => call<T.AiProposal>("ai.proposal_undo", { proposal_id }),
  },
  ocr: {
    retry: (kind: "invoice" | "payment", id: string) => call<void>("ocr.retry", { kind, id }),
  },
  loyalty: {
    customer: (customer_id: string) => call<T.LoyaltyCustomer>("loyalty.customer", { customer_id }),
    adjust: (customer_id: string, points: number, note: string, operation_id?: string) =>
      call<T.LoyaltyCustomer>("loyalty.adjust", { customer_id, points, note, operation_id }),
  },
  locations: {
    list: () => call<T.StockLocation[]>("locations.list"),
    save: (location_id: string | null, location: { code: string; name: string; active: boolean }) =>
      call<T.StockLocation[]>("locations.save", { location_id, location }),
    stock: (location_id: string) =>
      call<{ product_id: string; name: string; qty_milli: number }[]>("locations.stock", { location_id }),
  },
  transfers: {
    list: (status?: string) => call<T.Transfer[]>("transfers.list", { status }),
    get: (transfer_id: string) => call<T.Transfer>("transfers.get", { transfer_id }),
    create: (a: {
      to_branch_id?: string | null;
      from_location_id?: string | null;
      to_location_id?: string | null;
      note?: string | null;
      lines: { product_id: string; qty_milli: number }[];
    }) => call<T.Transfer>("transfers.create", a),
    ship: (transfer_id: string, operation_id: string) =>
      call<T.Transfer>("transfers.ship", { transfer_id, operation_id }),
    receive: (transfer_id: string, operation_id: string) =>
      call<T.Transfer>("transfers.receive", { transfer_id, operation_id }),
    cancel: (transfer_id: string) => call<T.Transfer>("transfers.cancel", { transfer_id }),
    inTransit: () =>
      call<{ product_id: string; name: string; to_branch_id: string; to_branch_name: string; qty_milli: number }[]>(
        "transfers.in_transit",
      ),
  },
  orders: {
    list: (status?: string | null) => call<T.DigitalOrder[]>("orders.list", { status }),
    get: (order_id: string) => call<T.DigitalOrder>("orders.get", { order_id }),
    products: (q: string) =>
      call<{ product_id: string; name: string; sku: string; price_minor: number | null }[]>("orders.products", { q }),
    save: (order_id: string | null, order: T.OrderInput) => call<T.DigitalOrder>("orders.save", { order_id, order }),
    fromInbox: (seq: number) => call<T.DigitalOrder>("orders.from_inbox", { seq }),
    confirm: (order_id: string, ack: T.ConfirmAck = {}) => call<T.DigitalOrder>("orders.confirm", { order_id, ...ack }),
    flow: () => call<T.OrderFlow>("orders.flow"),
    cancel: (order_id: string, reason?: string | null) => call<T.DigitalOrder>("orders.cancel", { order_id, reason }),
    setPayment: (order_id: string, payment_state: T.OrderPaymentState) =>
      call<T.DigitalOrder>("orders.set_payment", { order_id, payment_state }),
    convert: (order_id: string, operation_id: string) => call<T.Cart>("orders.convert", { order_id, operation_id }),
  },
  branches: {
    list: () => call<T.Branch[]>("branches.list"),
    save: (
      branch_id: string | null,
      branch: { code: string; name: string; address?: string | null; phone?: string | null; active: boolean },
    ) => call<T.Branch[]>("branches.save", { branch_id, branch }),
    userGet: (user_id: string) => call<string[]>("branches.user_get", { user_id }),
    userSet: (user_id: string, branch_ids: string[]) => call<string[]>("branches.user_set", { user_id, branch_ids }),
    switchTo: (branch_id: string) => call<T.Session>("branches.switch", { branch_id }),
    prices: (product_id: string) => call<T.BranchPrice[]>("branches.prices", { product_id }),
    setPrice: (product_id: string, branch_id: string, amount_minor: number | null) =>
      call<T.BranchPrice[]>("branches.set_price", { product_id, branch_id, amount_minor }),
  },
  companion: {
    issue: (label?: string | null, hours?: number) =>
      call<{ token: string; id: string; expires_at: string; path: string }>("companion.issue", { label, hours }),
    tokens: () => call<T.CompanionToken[]>("companion.tokens"),
    revoke: (id: string) => call<T.CompanionToken[]>("companion.revoke", { id }),
  },
};

export type Api = typeof api;
export { ApiError } from "./transport";
