import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Banknote,
  CreditCard,
  Lock,
  MoreHorizontal,
  PauseCircle,
  Printer,
  RotateCcw,
  ScanBarcode,
  Settings2,
  Smartphone,
  Truck,
  UserRound,
  Users,
  X,
  ListRestart,
} from "lucide-react";
import { api } from "../../api";
import type { Cart, CategoryRow, PosSearchRow, SaleResult, ShiftSummary } from "../../api/types";
import { useSession } from "../../state/session";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { useToast } from "../../components/toast";
import { explain } from "../../lib/errors";
import { formatMoney, formatQty } from "../../lib/money";
import { formatClock } from "../../lib/time";
import { BurstDetector, DuplicateGuard, looksLikeBarcode } from "../../lib/scanner";
import { setSoundEnabled, sounds } from "../../lib/sound";
import { Banner, Button, Chip } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { ConnectionPill } from "./ConnectionPill";
import { CartPanel } from "./CartPanel";
import { PaymentModal, SaleSuccess } from "./PaymentModal";
import {
  CashEventDialog,
  CustomItemDialog,
  CustomerPicker,
  DeliveryQuickDialog,
  DiscountDialog,
  HeldCartsDialog,
  HoldDialog,
  PriceDialog,
  QtyDialog,
  RecentSalesDialog,
  UnknownBarcodeDialog,
  PrintQueueDialog,
} from "./dialogs";
import { RefundFlow } from "./RefundFlow";
import { ShiftClose } from "./ShiftScreens";

type ModalState =
  | { kind: "none" }
  | { kind: "pay"; method: string }
  | { kind: "success"; sale: SaleResult }
  | { kind: "unknown"; barcode: string }
  | { kind: "hold" }
  | { kind: "held" }
  | { kind: "customer" }
  | { kind: "qty"; lineId: string }
  | { kind: "discount"; lineId: string | null }
  | { kind: "price"; lineId: string }
  | { kind: "custom" }
  | { kind: "cash"; cashKind: "paid_in" | "paid_out" | "safe_drop" | "no_sale" }
  | { kind: "refund" }
  | { kind: "recent" }
  | { kind: "delivery"; saleId: string | null }
  | { kind: "close_shift" }
  | { kind: "print_queue" };

const EMPTY_CART: Cart = {
  cart_id: null,
  status: "active",
  customer: null,
  lines: [],
  totals: { subtotal_minor: 0, discount_minor: 0, tax_minor: 0, total_minor: 0, item_count_milli: 0 },
  cart_discount_minor: 0,
  cart_discount_bp: 0,
  hold_number: null,
  hold_note: null,
  version: 0,
  notices: [],
};

export function PosScreen({ shift, onShiftClosed, reloadShift }: { shift: ShiftSummary; onShiftClosed: () => void; reloadShift: () => Promise<void> }) {
  const { session, config, has, lock, setMode, logout, handleAuthError } = useSession();
  const toast = useToast();
  const approve = useApproval();
  const [cart, setCart] = useState<Cart>(EMPTY_CART);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<PosSearchRow[] | null>(null);
  const [sel, setSel] = useState(0);
  const [grid, setGrid] = useState<PosSearchRow[]>([]);
  const [categories, setCategories] = useState<CategoryRow[]>([]);
  const [activeCat, setActiveCat] = useState<string>("fav");
  const [selectedLine, setSelectedLine] = useState<string | null>(null);
  const [flashLine, setFlashLine] = useState<string | null>(null);
  const [modal, setModal] = useState<ModalState>({ kind: "none" });
  const [notice, setNotice] = useState<{ tone: "warning" | "danger" | "info"; text: string } | null>(null);
  const [lastSale, setLastSale] = useState<SaleResult | null>(null);
  const [moreOpen, setMoreOpen] = useState(false);
  const [clock, setClock] = useState(() => formatClock(new Date()));
  const scanRef = useRef<HTMLInputElement>(null);
  const burst = useRef(new BurstDetector());
  const dupGuard = useRef(new DuplicateGuard(0));
  const globalBuf = useRef("");

  useEffect(() => {
    setSoundEnabled(config?.pos.scan_sound ?? true);
    dupGuard.current.setWindow(config?.pos.duplicate_scan_window_ms ?? 0);
  }, [config]);

  const focusScan = useCallback(() => {
    requestAnimationFrame(() => scanRef.current?.focus());
  }, []);

  const fail = useCallback(
    (e: unknown) => {
      if (e instanceof ApprovalCancelled) return;
      if (handleAuthError(e)) return;
      const ex = explain(e);
      sounds.error();
      setNotice({ tone: "danger", text: `${ex.message} ${ex.action}`.trim() });
    },
    [handleAuthError],
  );

  const applyCart = useCallback((c: Cart, changedLine?: string | null) => {
    setCart(c);
    if (c.notices.length) setNotice({ tone: "warning", text: c.notices.join(" ") });
    if (changedLine) {
      setSelectedLine(changedLine);
      setFlashLine(changedLine);
      setTimeout(() => setFlashLine(null), 700);
    }
  }, []);

  // Initial load: current cart, categories and quick products.
  useEffect(() => {
    api.pos.cart().then((c) => applyCart(c)).catch(fail);
    api.categories
      .list()
      .then(setCategories)
      .catch(() => {});
    focusScan();
  }, [applyCart, fail, focusScan]);

  useEffect(() => {
    const opts = activeCat === "fav" ? { favorites: true, limit: 60 } : activeCat === "all" ? { limit: 60 } : { category_id: activeCat, limit: 120 };
    api.pos
      .search("", opts)
      .then((r) => {
        // If there are no favourites yet, fall back to all products.
        if (activeCat === "fav" && r.length === 0) return api.pos.search("", { limit: 60 }).then(setGrid);
        setGrid(r);
      })
      .catch(() => {});
  }, [activeCat]);

  useEffect(() => {
    const t = setInterval(() => setClock(formatClock(new Date())), 15000);
    return () => clearInterval(t);
  }, []);

  // Debounced product search (name / SKU / barcode). Barcode scans never go through fuzzy search.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults(null);
      return;
    }
    const t = setTimeout(() => {
      api.pos
        .search(q, { limit: 40 })
        .then((r) => {
          setResults(r);
          setSel(0);
        })
        .catch(() => {});
    }, 120);
    return () => clearTimeout(t);
  }, [query]);

  // Scans are processed strictly in order. A scan is never dropped while a
  // previous one is in flight, and the field is cleared the moment Enter is
  // pressed so the next barcode can be typed immediately.
  const scanQueue = useRef<Promise<void>>(Promise.resolve());
  const processScan = useCallback(
    async (raw: string) => {
      let code = raw.trim();
      let qty: number | undefined;
      const m = /^(\d+(?:\.\d{1,3})?)\*(\S+)$/.exec(code);
      if (m) {
        const parts = m[1].split(".");
        qty = Number(parts[0]) * 1000 + Number(((parts[1] ?? "") + "000").slice(0, 3));
        code = m[2];
      }
      if (!dupGuard.current.accept(code)) return;
      try {
        const r = await api.pos.scan(code, qty);
        if (r.outcome === "added") {
          sounds.scan();
          setNotice(null);
          applyCart(r.cart, r.line_id);
        } else if (r.outcome === "unknown") {
          sounds.unknown();
          setModal({ kind: "unknown", barcode: r.barcode });
        } else {
          sounds.unknown();
          setNotice({ tone: "warning", text: `${r.product_name ?? "This product"} is archived and cannot be sold. Ask a manager.` });
        }
      } catch (e) {
        fail(e);
      }
    },
    [applyCart, fail],
  );
  const doScan = useCallback(
    (raw: string) => {
      setQuery("");
      setResults(null);
      burst.current.reset();
      scanQueue.current = scanQueue.current.then(() => processScan(raw));
      return scanQueue.current;
    },
    [processScan],
  );

  const addProduct = useCallback(
    async (p: PosSearchRow) => {
      try {
        const c = await api.pos.addProduct(p.product_id);
        sounds.scan();
        const line = c.lines.filter((l) => l.product_id === p.product_id).at(-1);
        applyCart(c, line?.line_id);
        setNotice(null);
        setQuery("");
        setResults(null);
      } catch (e) {
        fail(e);
      }
      focusScan();
    },
    [applyCart, fail, focusScan],
  );

  const onScanKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key.length === 1) burst.current.key();
    if (e.key === "Enter") {
      e.preventDefault();
      const v = query;
      const isBurst = burst.current.isBurst(Math.min(v.length, 8));
      if (/^\d+(?:\.\d{1,3})?\*\S+$/.test(v.trim()) || looksLikeBarcode(v, isBurst)) {
        void doScan(v);
      } else if (results && results[sel]) {
        void addProduct(results[sel]);
      }
      return;
    }
    if (results && results.length) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSel((s) => Math.min(results.length - 1, s + 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSel((s) => Math.max(0, s - 1));
      }
    } else if (!query && cart.lines.length) {
      const idx = cart.lines.findIndex((l) => l.line_id === selectedLine);
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedLine(cart.lines[Math.min(cart.lines.length - 1, idx + 1)].line_id);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedLine(cart.lines[Math.max(0, idx - 1)].line_id);
      }
    }
    if (e.key === "Escape") {
      setQuery("");
      setResults(null);
    }
  };

  const lineAction = useCallback(
    async (fn: (approval: string | null) => Promise<Cart>) => {
      try {
        applyCart(await approve(fn));
      } catch (e) {
        fail(e);
      }
      focusScan();
    },
    [approve, applyCart, fail, focusScan],
  );

  const changeQty = (lineId: string, delta: number) => {
    const l = cart.lines.find((x) => x.line_id === lineId);
    if (!l) return;
    const next = l.qty_milli + delta * 1000;
    if (next <= 0) {
      void lineAction((tok) => api.pos.removeLine(lineId, tok));
      return;
    }
    void lineAction((tok) => api.pos.setQty(lineId, next, tok));
  };

  const removeLine = (lineId: string) => void lineAction((tok) => api.pos.removeLine(lineId, tok));

  const hasLines = cart.lines.length > 0;
  const tenders = config?.payments ?? [];
  const tenderEnabled = (m: string) => tenders.some((t) => t.method === m);

  const openPay = useCallback(
    (method: string) => {
      if (!cart.cart_id || !cart.lines.length) {
        setNotice({ tone: "info", text: "Scan an item before taking payment." });
        return;
      }
      setModal({ kind: "pay", method });
    },
    [cart],
  );

  // Global shortcuts and scanner capture when focus is outside the scan field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (modal.kind !== "none") return;
      const target = e.target as HTMLElement;
      const inField = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT");
      const fkeys: Record<string, () => void> = {
        F2: () => focusScan(),
        F3: () => setModal({ kind: "customer" }),
        F4: () => hasLines && setModal({ kind: "hold" }),
        F5: () => setModal({ kind: "held" }),
        F6: () => openPay("cash"),
        F7: () => tenderEnabled("card") && openPay("card"),
        F8: () => tenderEnabled("benefitpay") && openPay("benefitpay"),
        F9: () => openPay(tenders[0]?.method ?? "cash"),
        F10: () => setMoreOpen((v) => !v),
      };
      if (fkeys[e.key]) {
        e.preventDefault();
        fkeys[e.key]();
        return;
      }
      if (e.ctrlKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        void lock();
        return;
      }
      if (e.key === "Delete" && selectedLine && !query) {
        e.preventDefault();
        removeLine(selectedLine);
        return;
      }
      if (!inField && selectedLine && (e.key === "+" || e.key === "-")) {
        e.preventDefault();
        changeQty(selectedLine, e.key === "+" ? 1 : -1);
        return;
      }
      if (!inField) {
        // Scanner burst while focus is elsewhere: capture and route to scan.
        if (e.key.length === 1) {
          burst.current.key();
          globalBuf.current += e.key;
        } else if (e.key === "Enter" && globalBuf.current) {
          const v = globalBuf.current;
          globalBuf.current = "";
          if (looksLikeBarcode(v, burst.current.isBurst(Math.min(v.length, 8)))) void doScan(v);
        }
      } else {
        globalBuf.current = "";
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const closeModal = () => {
    setModal({ kind: "none" });
    focusScan();
  };

  const afterSale = (sale: SaleResult) => {
    sounds.success();
    setLastSale(sale);
    setCart(EMPTY_CART);
    setSelectedLine(null);
    setNotice(null);
    setModal({ kind: "success", sale });
    void reloadShift();
  };

  const cancelSale = async () => {
    try {
      applyCart(await approve((tok) => api.pos.cancel(tok)));
      setSelectedLine(null);
    } catch (e) {
      fail(e);
    }
    focusScan();
  };

  const doLogout = async () => {
    if (hasLines) {
      setNotice({ tone: "warning", text: "A sale is currently in progress. Hold it or cancel it before logging out." });
      return;
    }
    await logout();
  };

  const selected = useMemo(() => cart.lines.find((l) => l.line_id === selectedLine) ?? null, [cart, selectedLine]);
  const printFailed = lastSale?.print?.status === "failed";

  return (
    <div className="pos-root" data-testid="pos">
      <header className="pos-header">
        <div className="brand">
          <Logo size={28} /> AMWAPOS
        </div>
        <div className="sep" />
        <div className="hitem">{config?.business_name}</div>
        <div className="grow" style={{ display: "flex", justifyContent: "center" }}>
          {cart.customer ? (
            <span className="customer-pill">
              <UserRound size={14} style={{ verticalAlign: -2 }} /> {cart.customer.name}
            </span>
          ) : null}
        </div>
        <ConnectionPill />
        <span className="status-pill" title={config?.printer_configured ? "Receipt printer configured" : "No receipt printer configured"}>
          <Printer size={13} /> {config?.printer_configured ? (printFailed ? "Print failed" : "Printer") : "No printer"}
        </span>
        <div className="hitem">Shift {shift.shift_number}</div>
        <div className="hitem">
          <UserRound size={15} /> {session?.display_name}
        </div>
        <div className="hitem num" style={{ fontWeight: 650, color: "#fff" }}>
          {clock}
        </div>
        <Button size="sm" icon={<Lock size={15} />} onClick={() => void lock()} title="Lock (Ctrl+L)">
          Lock
        </Button>
      </header>
      <main className="pos-main">
        <section className="pos-left">
          <div className="scan-box">
            <ScanBarcode size={20} className="scan-icon" aria-hidden />
            <input
              ref={scanRef}
              className="input"
              placeholder="Scan barcode or search product…"
              aria-label="Scan barcode or search product"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onScanKey}
              autoComplete="off"
              spellCheck={false}
              data-testid="scan-input"
            />
            {query ? (
              <Button variant="ghost" size="sm" className="clear" aria-label="Clear search" icon={<X size={16} />} onClick={() => (setQuery(""), focusScan())} />
            ) : null}
          </div>
          {notice ? (
            <Banner tone={notice.tone} action={<Button variant="ghost" size="sm" aria-label="Dismiss" icon={<X size={14} />} onClick={() => setNotice(null)} />}>
              {notice.text}
            </Banner>
          ) : null}
          {printFailed && modal.kind === "none" ? (
            <Banner
              tone="warning"
              title={`Sale ${lastSale!.receipt_number} completed — receipt could not be printed`}
              action={
                <Button
                  size="sm"
                  onClick={async () => {
                    const r = await api.print.retry(lastSale!.print!.job_id!);
                    setLastSale({ ...lastSale!, print: r });
                    if (r.status === "printed") toast("success", "Receipt printed");
                  }}
                >
                  Retry Print
                </Button>
              }
            >
              {lastSale!.print!.message}
            </Banner>
          ) : null}
          <div className="pos-panel">
            {results ? (
              <div className="results" role="listbox" aria-label="Search results">
                {results.length === 0 ? (
                  <div className="empty">
                    <h3>No products found</h3>
                    <p>Check the spelling or scan the barcode.</p>
                  </div>
                ) : (
                  results.map((r, i) => (
                    <div key={r.product_id} role="option" aria-selected={i === sel} className={`result-row ${i === sel ? "sel" : ""}`} onMouseDown={(e) => (e.preventDefault(), void addProduct(r))}>
                      <div className="grow">
                        <div style={{ fontWeight: 600 }}>{r.name}</div>
                        <div className="tiny">
                          {r.category_name ?? "—"} · SKU {r.sku} {r.primary_barcode ? `· ${r.primary_barcode}` : ""}
                        </div>
                      </div>
                      <div className="tiny" style={{ minWidth: 70, textAlign: "right" }}>
                        {r.track_inventory ? `Stock ${formatQty(r.stock_milli)}` : ""}
                      </div>
                      <div className="r-price">{r.price_minor === null ? "No price" : formatMoney(r.price_minor)}</div>
                    </div>
                  ))
                )}
              </div>
            ) : (
              <>
                <div className="cat-chips" role="tablist" aria-label="Categories">
                  <button className={`filter-chip ${activeCat === "fav" ? "active" : ""}`} onClick={() => setActiveCat("fav")}>
                    Favorites
                  </button>
                  <button className={`filter-chip ${activeCat === "all" ? "active" : ""}`} onClick={() => setActiveCat("all")}>
                    All
                  </button>
                  {categories
                    .filter((c) => c.product_count > 0)
                    .map((c) => (
                      <button key={c.category_id} className={`filter-chip ${activeCat === c.category_id ? "active" : ""}`} onClick={() => setActiveCat(c.category_id)}>
                        {c.name}
                      </button>
                    ))}
                </div>
                <div className="tile-grid">
                  {grid.map((p) => (
                    <button key={p.product_id} className="p-tile" onClick={() => void addProduct(p)} title={p.name}>
                      <span className="p-name">{p.name}</span>
                      <span className="tiny">{p.sku}</span>
                      <span className="p-price">{p.price_minor === null ? "—" : formatMoney(p.price_minor)}</span>
                    </button>
                  ))}
                  {grid.length === 0 ? (
                    <div className="empty" style={{ gridColumn: "1 / -1" }}>
                      <h3>Ready to scan</h3>
                      <p>Scan a barcode or type a product name.</p>
                    </div>
                  ) : null}
                </div>
              </>
            )}
          </div>
          <div className="quick-actions" style={{ position: "relative" }}>
            <Button icon={<Users size={20} />} onClick={() => setModal({ kind: "customer" })} title="F3">
              Customer
            </Button>
            <Button icon={<PauseCircle size={20} />} onClick={() => setModal({ kind: "hold" })} disabled={!hasLines || !has("pos.hold")} title="F4">
              Hold
            </Button>
            <Button icon={<ListRestart size={20} />} onClick={() => setModal({ kind: "held" })} disabled={!has("pos.hold")} title="F5">
              Held
            </Button>
            <Button icon={<RotateCcw size={20} />} onClick={() => setModal({ kind: "refund" })} title="Refund">
              Refund
            </Button>
            <Button icon={<MoreHorizontal size={20} />} onClick={() => setMoreOpen((v) => !v)} aria-expanded={moreOpen} title="F10">
              More
            </Button>
            {moreOpen ? (
              <div className="menu" style={{ bottom: 64, top: "auto", right: 0 }} role="menu" onMouseLeave={() => setMoreOpen(false)}>
                {[
                  { label: "Custom item", show: has("pos.custom_item") || config?.pos.allow_custom_item, run: () => setModal({ kind: "custom" }) },
                  { label: "Sale discount", show: hasLines, run: () => setModal({ kind: "discount", lineId: null }) },
                  { label: "Reprint / recent sales", show: has("pos.reprint"), run: () => setModal({ kind: "recent" }) },
                  { label: "Print queue", show: true, run: () => setModal({ kind: "print_queue" }) },
                  { label: "Delivery for last sale", show: !!lastSale, run: () => setModal({ kind: "delivery", saleId: lastSale?.sale_id ?? null }) },
                  { label: "Open drawer (no sale)", show: true, run: () => setModal({ kind: "cash", cashKind: "no_sale" }) },
                  { label: "Paid in", show: true, run: () => setModal({ kind: "cash", cashKind: "paid_in" }) },
                  { label: "Paid out", show: true, run: () => setModal({ kind: "cash", cashKind: "paid_out" }) },
                  { label: "Safe drop", show: true, run: () => setModal({ kind: "cash", cashKind: "safe_drop" }) },
                  { label: "Cancel sale", show: hasLines, run: () => void cancelSale() },
                  { label: "Close shift", show: has("shift.close"), run: () => setModal({ kind: "close_shift" }) },
                  { label: "Admin", show: has("admin.access"), run: () => setMode("admin") },
                  { label: "Lock terminal", show: true, run: () => void lock() },
                  { label: "Logout", show: true, run: () => void doLogout() },
                ]
                  .filter((i) => i.show)
                  .map((i) => (
                    <button
                      key={i.label}
                      role="menuitem"
                      onClick={() => {
                        setMoreOpen(false);
                        i.run();
                      }}
                    >
                      {i.label === "Admin" ? <Settings2 size={15} /> : i.label.startsWith("Delivery") ? <Truck size={15} /> : null}
                      {i.label}
                    </button>
                  ))}
              </div>
            ) : null}
          </div>
        </section>
        <section className="pos-right">
          <CartPanel
            cart={cart}
            selectedLine={selectedLine}
            flashLine={flashLine}
            onSelect={setSelectedLine}
            onQty={changeQty}
            onRemove={removeLine}
            onEditQty={(id) => setModal({ kind: "qty", lineId: id })}
            onDiscount={(id) => setModal({ kind: "discount", lineId: id })}
            onPrice={(id) => setModal({ kind: "price", lineId: id })}
            canPriceOverride
          />
          <div className="pos-panel" style={{ flex: "none" }}>
            <div className="totals">
              <div className="t-row">
                <span>Subtotal</span>
                <span>{formatMoney(cart.totals.subtotal_minor)}</span>
              </div>
              <div className="t-row">
                <span>Discount</span>
                <span>{cart.totals.discount_minor ? formatMoney(-cart.totals.discount_minor) : formatMoney(0)}</span>
              </div>
              <div className="t-row">
                <span>VAT {cart.lines.some((l) => l.tax_inclusive) ? "(included)" : ""}</span>
                <span>{formatMoney(cart.totals.tax_minor)}</span>
              </div>
              <div className="t-total">
                <span style={{ fontWeight: 700, color: "var(--text-2)" }}>TOTAL</span>
                <span className="amount" data-testid="cart-total">
                  {formatMoney(cart.totals.total_minor)}
                </span>
              </div>
              <div className="row pay-btn">
                <Button size="xl" variant="primary" className="grow" onClick={() => openPay(tenders[0]?.method ?? "cash")} disabled={!hasLines} data-testid="pay">
                  PAY {formatMoney(cart.totals.total_minor)}
                </Button>
              </div>
              <div className="row" style={{ marginTop: 8 }}>
                <Button className="grow" icon={<Banknote size={16} />} kbd="F6" onClick={() => openPay("cash")} disabled={!hasLines}>
                  Cash
                </Button>
                {tenderEnabled("card") ? (
                  <Button className="grow" icon={<CreditCard size={16} />} kbd="F7" onClick={() => openPay("card")} disabled={!hasLines}>
                    Card
                  </Button>
                ) : null}
                {tenderEnabled("benefitpay") ? (
                  <Button className="grow" icon={<Smartphone size={16} />} kbd="F8" onClick={() => openPay("benefitpay")} disabled={!hasLines}>
                    BenefitPay
                  </Button>
                ) : null}
              </div>
            </div>
          </div>
          {selected && selected.stock_milli !== null && selected.stock_milli <= 0 ? (
            <Chip tone="warning">
              {selected.name}: recorded stock {formatQty(selected.stock_milli)}
            </Chip>
          ) : null}
        </section>
      </main>

      {modal.kind === "pay" && cart.cart_id ? (
        <PaymentModal cart={cart} initialMethod={modal.method} tenders={tenders} onClose={closeModal} onPaid={afterSale} onCartChanged={applyCart} />
      ) : null}
      {modal.kind === "success" ? (
        <SaleSuccess
          sale={modal.sale}
          returnSeconds={config?.pos.return_to_scan_seconds ?? 4}
          onClose={closeModal}
          onReprint={async () => {
            try {
              const r = await api.sales.reprint(modal.sale.sale_id);
              setLastSale({ ...modal.sale, print: r });
              toast(r.status === "printed" ? "success" : "warning", r.status === "printed" ? "Receipt reprinted" : "Receipt not printed", r.message ?? undefined);
            } catch (e) {
              fail(e);
            }
          }}
          onDelivery={() => setModal({ kind: "delivery", saleId: modal.sale.sale_id })}
          onRetryPrint={async (jobId) => {
            const r = await api.print.retry(jobId);
            setLastSale({ ...modal.sale, print: r });
            return r;
          }}
        />
      ) : null}
      {modal.kind === "unknown" ? (
        <UnknownBarcodeDialog
          barcode={modal.barcode}
          canCustom={!!config?.pos.allow_custom_item}
          onClose={closeModal}
          onSearch={() => {
            closeModal();
          }}
          onCustom={() => setModal({ kind: "custom" })}
        />
      ) : null}
      {modal.kind === "hold" ? (
        <HoldDialog
          cart={cart}
          onClose={closeModal}
          onHeld={() => {
            setCart(EMPTY_CART);
            setSelectedLine(null);
            toast("success", "Sale held");
            closeModal();
          }}
        />
      ) : null}
      {modal.kind === "held" ? (
        <HeldCartsDialog
          onClose={closeModal}
          onRestored={(c) => {
            applyCart(c);
            closeModal();
          }}
          currentHasLines={hasLines}
        />
      ) : null}
      {modal.kind === "customer" ? (
        <CustomerPicker
          current={cart.customer}
          onClose={closeModal}
          onPicked={(c) => {
            applyCart(c);
            closeModal();
          }}
        />
      ) : null}
      {modal.kind === "qty" ? (
        <QtyDialog line={cart.lines.find((l) => l.line_id === modal.lineId)!} onClose={closeModal} onApply={(q) => (closeModal(), lineAction((tok) => api.pos.setQty(modal.lineId, q, tok)))} />
      ) : null}
      {modal.kind === "discount" ? (
        <DiscountDialog
          line={modal.lineId ? cart.lines.find((l) => l.line_id === modal.lineId) ?? null : null}
          cart={cart}
          maxBp={config?.pos.cashier_max_discount_bp ?? 1000}
          onClose={closeModal}
          onApply={(minor, bp) => {
            closeModal();
            const lineId = modal.lineId;
            void lineAction((tok) => (lineId ? api.pos.lineDiscount(lineId, minor, bp, tok) : api.pos.cartDiscount(minor, bp, tok)));
          }}
        />
      ) : null}
      {modal.kind === "price" ? (
        <PriceDialog
          line={cart.lines.find((l) => l.line_id === modal.lineId)!}
          onClose={closeModal}
          onApply={(price, reason) => {
            closeModal();
            void lineAction((tok) => api.pos.priceOverride(modal.lineId, price, reason, tok));
          }}
        />
      ) : null}
      {modal.kind === "custom" ? (
        <CustomItemDialog
          onClose={closeModal}
          onAdded={(c) => {
            applyCart(c, c.lines.at(-1)?.line_id);
            closeModal();
          }}
        />
      ) : null}
      {modal.kind === "cash" ? <CashEventDialog kind={modal.cashKind} onClose={closeModal} onDone={() => (closeModal(), void reloadShift())} /> : null}
      {modal.kind === "refund" ? <RefundFlow onClose={closeModal} onDone={() => void reloadShift()} /> : null}
      {modal.kind === "recent" ? <RecentSalesDialog onClose={closeModal} /> : null}
      {modal.kind === "print_queue" ? <PrintQueueDialog onClose={closeModal} /> : null}
      {modal.kind === "delivery" ? <DeliveryQuickDialog saleId={modal.saleId} customer={cart.customer} onClose={closeModal} /> : null}
      {modal.kind === "close_shift" ? (
        <ShiftClose
          shiftId={shift.shift_id}
          onClose={closeModal}
          onClosed={() => {
            setModal({ kind: "none" });
            onShiftClosed();
          }}
        />
      ) : null}
      <span className="sr-only" aria-live="polite">
        {cart.lines.length} items, total {formatMoney(cart.totals.total_minor)}
      </span>
    </div>
  );
}
