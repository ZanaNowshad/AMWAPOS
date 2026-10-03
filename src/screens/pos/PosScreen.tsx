import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Bot,
  DoorClosed,
  Inbox,
  Languages,
  ListRestart,
  Lock,
  LogOut,
  MoreHorizontal,
  PauseCircle,
  Percent,
  PlusSquare,
  Printer,
  ReceiptText,
  RotateCcw,
  ScanBarcode,
  Settings2,
  ShoppingBag,
  Truck,
  UserPlus,
  UserRound,
  Users,
  Vault,
  X,
  XCircle,
} from "lucide-react";
import { api } from "../../api";
import type { Cart, PosSearchRow, SaleResult, ShiftSummary } from "../../api/types";
import { useSession } from "../../state/session";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { useToast } from "../../components/toast";
import { explain } from "../../lib/errors";
import { formatMoney, formatQty } from "../../lib/money";
import { formatClock } from "../../lib/time";
import { BurstDetector, DuplicateGuard, looksLikeBarcode } from "../../lib/scanner";
import { setSoundEnabled, sounds } from "../../lib/sound";
import { Banner, Button, Modal } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { ConnectionPill } from "./ConnectionPill";
import { AiChat, AiReady } from "../admin/aiChat";
import { HashRouter } from "react-router-dom";
import type { AiContext } from "../../api/types";
import { CartPanel } from "./CartPanel";
import { initials } from "../login/Login";
import { ProductImage } from "../../components/ProductImage";
import { PaymentModal, SaleSuccess } from "./PaymentModal";
import { SendRail, TicketSheet } from "./SendLoop";
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
  LoyaltyRedeemDialog,
} from "./dialogs";
import { OrdersList } from "../orders";
import { useFeature } from "../../components/FeatureGate";
import { RefundFlow } from "./RefundFlow";
import { ShiftClose } from "./ShiftScreens";
import { getLang, switchLang, t, tb } from "../../i18n";
import { BackupPill } from "../../components/BackupAlert";

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
  | { kind: "print_queue" }
  | { kind: "price_changes"; notices: string[] }
  | { kind: "redeem" }
  | { kind: "orders" };

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

export function PosScreen({
  shift,
  onShiftClosed,
  reloadShift,
}: {
  shift: ShiftSummary;
  onShiftClosed: () => void;
  reloadShift: () => Promise<void>;
}) {
  const { session, config, has, lock, setMode, logout, handleAuthError } = useSession();
  const toast = useToast();
  const approve = useApproval();
  const [cart, setCart] = useState<Cart>(EMPTY_CART);
  // The assistant drawer is open: till shortcuts pause while it has the keyboard.
  const [aiOpen, setAiOpen] = useState(false);
  // The Send rail shares the assistant's side; only one of the two is open.
  const [railOpen, setRailOpen] = useState(false);
  const [ticketId, setTicketId] = useState<string | null>(null);
  const [sendBadge, setSendBadge] = useState(0);
  const [railKey, setRailKey] = useState(0);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<PosSearchRow[] | null>(null);
  const [sel, setSel] = useState(0);
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

  // Initial load: the current cart. The till shows no catalogue grid: scan or search.
  useEffect(() => {
    api.pos
      .cart()
      .then((c) => applyCart(c))
      .catch(fail);
    focusScan();
  }, [applyCart, fail, focusScan]);

  useEffect(() => {
    const tv = setInterval(() => setClock(formatClock(new Date())), 15000);
    return () => clearInterval(tv);
  }, []);

  // Debounced product search (name / SKU / barcode). Barcode scans never go through fuzzy search.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults(null);
      return;
    }
    const tv = setTimeout(() => {
      api.pos
        .search(q, { limit: 40 })
        .then((r) => {
          setResults(r);
          setSel(0);
        })
        .catch(() => {});
    }, 120);
    return () => clearTimeout(tv);
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
          setNotice({
            tone: "warning",
            text: t("{0} is archived and cannot be sold. Ask a manager.", r.product_name ?? "This product"),
          });
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
  const ordersOn = useFeature("orders.digital");
  // Held ticket numbers for this till (shown on the Held button).
  const [heldTickets, setHeldTickets] = useState<number[]>([]);
  const reloadHeld = useCallback(async () => {
    if (!has("pos.hold")) return;
    try {
      const rows = await api.pos.held();
      setHeldTickets(rows.filter((r) => !r.locked && r.hold_number !== null).map((r) => r.hold_number as number));
    } catch {
      // The count is a convenience; the Held dialog shows errors.
    }
  }, [has]);
  useEffect(() => {
    void reloadHeld();
  }, [reloadHeld, cart.cart_id]);
  const tenders = config?.payments ?? [];
  const tenderEnabled = (m: string) => tenders.some((tv) => tv.method === m);

  const openPay = useCallback(
    (method: string) => {
      if (!cart.cart_id || !cart.lines.length) {
        setNotice({ tone: "info", text: t("Scan an item before taking payment.") });
        return;
      }
      setModal({ kind: "pay", method });
    },
    [cart],
  );

  // Esc closes the till assistant (sheets opened from it handle Esc themselves).
  useEffect(() => {
    if (!aiOpen) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || document.querySelector(".backdrop")) return;
      setAiOpen(false);
      focusScan();
    };
    window.addEventListener("keydown", onEsc);
    return () => window.removeEventListener("keydown", onEsc);
  }, [aiOpen, focusScan]);

  // Esc closes the Send rail (the ticket sheet handles its own Esc).
  useEffect(() => {
    if (!railOpen) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || document.querySelector(".backdrop")) return;
      setRailOpen(false);
      focusScan();
    };
    window.addEventListener("keydown", onEsc);
    return () => window.removeEventListener("keydown", onEsc);
  }, [railOpen, focusScan]);

  // Global shortcuts and scanner capture when focus is outside the scan field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (modal.kind !== "none" || aiOpen || moreOpen || ticketId) return;
      const target = e.target as HTMLElement;
      const inField =
        target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT");
      // Shortcuts stay live in the scan field (always focused on a till) and
      // are dead in any other text field, so typing never triggers them.
      const inOtherField = inField && target !== scanRef.current;
      if (inOtherField) return;
      // "/" or Ctrl+K: product search (the scan field also searches by name).
      if ((e.key === "/" && !inField) || (e.ctrlKey && e.key.toLowerCase() === "k")) {
        e.preventDefault();
        focusScan();
        return;
      }
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

  const reloadSends = useCallback(async () => {
    try {
      setSendBadge((await api.tickets.counts()).badge);
    } catch {
      // The badge is a convenience; the rail shows errors.
    }
    setRailKey((k) => k + 1);
  }, []);
  useEffect(() => {
    void reloadSends();
    const tv = setInterval(() => void reloadSends(), 60_000);
    return () => clearInterval(tv);
  }, [reloadSends]);

  const afterSale = (sale: SaleResult) => {
    if (sale.delivery_id) void reloadSends();
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
      setNotice({
        tone: "warning",
        text: t("A sale is currently in progress. Hold it or cancel it before logging out."),
      });
      return;
    }
    await logout();
  };

  const selected = useMemo(() => cart.lines.find((l) => l.line_id === selectedLine) ?? null, [cart, selectedLine]);
  // F6: the full assistant at the till, with the open cart as context.
  const aiOn = useFeature("ai.enabled") && has("ai.use");
  const cartContext = useCallback(
    (): AiContext | null =>
      cart.lines.length
        ? {
            kind: "cart",
            lines: cart.lines.map((l) => ({
              product_id: l.product_id,
              name: l.name,
              qty_milli: l.qty_milli,
              unit_price_minor: l.unit_price_minor,
              line_total_minor: l.line_total_minor,
            })),
            total_minor: cart.totals.total_minor,
            customer: cart.customer?.name ?? null,
            held_ticket: cart.hold_number ? String(cart.hold_number) : null,
          }
        : null,
    [cart],
  );
  const printFailed = lastSale?.print?.status === "failed";

  const moreItems: { label: string; show: boolean | undefined; icon?: React.ReactNode; run: () => void }[] = [
    { label: t("Admin"), show: has("admin.access"), icon: <Settings2 size={20} />, run: () => setMode("admin") },
    {
      label: getLang() === "ar" ? "English" : "العربية",
      show: true,
      icon: <Languages size={20} />,
      run: () => switchLang(getLang() === "ar" ? "en" : "ar"),
    },
    { label: t("Lock terminal"), show: true, icon: <Lock size={20} />, run: () => void lock() },
    {
      label: t("Delivery for last sale"),
      show: !!lastSale,
      icon: <Truck size={20} />,
      run: () => setModal({ kind: "delivery", saleId: lastSale?.sale_id ?? null }),
    },
    {
      label: t("Orders"),
      show: ordersOn,
      icon: <ShoppingBag size={20} />,
      run: () => setModal({ kind: "orders" }),
    },
    {
      label: t("Custom item"),
      show: has("pos.custom_item") || config?.pos.allow_custom_item,
      icon: <PlusSquare size={20} />,
      run: () => setModal({ kind: "custom" }),
    },
    {
      label: t("Reprint / recent sales"),
      show: has("pos.reprint"),
      icon: <ReceiptText size={20} />,
      run: () => setModal({ kind: "recent" }),
    },
    { label: t("Print queue"), show: true, icon: <Printer size={20} />, run: () => setModal({ kind: "print_queue" }) },
    {
      label: t("Open drawer (no sale)"),
      show: true,
      icon: <Inbox size={20} />,
      run: () => setModal({ kind: "cash", cashKind: "no_sale" }),
    },
    {
      label: t("Paid in"),
      show: true,
      icon: <ArrowDownToLine size={20} />,
      run: () => setModal({ kind: "cash", cashKind: "paid_in" }),
    },
    {
      label: t("Paid out"),
      show: true,
      icon: <ArrowUpFromLine size={20} />,
      run: () => setModal({ kind: "cash", cashKind: "paid_out" }),
    },
    {
      label: t("Safe drop"),
      show: true,
      icon: <Vault size={20} />,
      run: () => setModal({ kind: "cash", cashKind: "safe_drop" }),
    },
    { label: t("Cancel sale"), show: hasLines, icon: <XCircle size={20} />, run: () => void cancelSale() },
    {
      label: t("Close shift"),
      show: has("shift.close"),
      icon: <DoorClosed size={20} />,
      run: () => setModal({ kind: "close_shift" }),
    },
    { label: t("Logout"), show: true, icon: <LogOut size={20} />, run: () => void doLogout() },
  ];

  return (
    <div className="pos-root" data-testid="pos">
      <header className="pos-header">
        <div className="brand" title={config?.business_name ?? undefined}>
          <Logo size={28} />
          <span className="brand-name ellipsis" dir="auto">
            {config?.business_name || t("AMWAPOS")}
          </span>
        </div>
        <span className="shift-chip">
          <UserRound size={16} aria-hidden />
          <span className="ellipsis" dir="auto">
            {session?.display_name}
          </span>
          <span className="shift-no">{shift.shift_number}</span>
        </span>
        <div className="pills">
          <BackupPill />
          <ConnectionPill />
          <span className={`status-pill ${printFailed ? "err" : config?.printer_configured ? "" : "warn"}`}>
            <Printer size={14} aria-hidden />
            {config?.printer_configured ? (printFailed ? t("Print failed") : t("Printer")) : t("No printer")}
          </span>
          {shift.safe_drop_minor > 0 ? (
            <span className="status-pill">{t("Dropped {0}", formatMoney(shift.safe_drop_minor))}</span>
          ) : null}
        </div>
        <span className="grow" />
        {has("pos.hold") ? (
          <button
            type="button"
            className={`top-btn held-btn ${heldTickets.length ? "has" : ""}`}
            onClick={() => setModal({ kind: "held" })}
            aria-label={
              heldTickets.length ? t("Held tickets: {0}", heldTickets.map((n) => `#${n}`).join(" ")) : t("Held tickets")
            }
          >
            <ListRestart size={20} aria-hidden />
            <span className="top-label">{t("Held")}</span>
            {heldTickets.length ? (
              <span className="badge" data-testid="held-count">
                {heldTickets.length}
              </span>
            ) : null}
          </button>
        ) : null}
        <span className="clock num" aria-label={t("Time")}>
          {clock}
        </span>
        <button
          type="button"
          className={`top-btn icon-only ${railOpen ? "on" : ""}`}
          data-testid="send-btn"
          aria-label={sendBadge ? t("Send: {0} open", sendBadge) : t("Send")}
          aria-pressed={railOpen}
          onClick={() => (setRailOpen((v) => !v), setAiOpen(false))}
        >
          <Truck size={22} aria-hidden />
          {sendBadge ? (
            <span className="badge corner" data-testid="send-badge">
              {sendBadge}
            </span>
          ) : null}
        </button>
        {aiOn ? (
          <button
            type="button"
            className={`top-btn icon-only ${aiOpen ? "on" : ""}`}
            data-testid="till-ai"
            aria-label={t("Assistant")}
            aria-pressed={aiOpen}
            onClick={() => (setAiOpen((v) => !v), setRailOpen(false))}
          >
            <Bot size={22} aria-hidden />
          </button>
        ) : null}
        <button
          type="button"
          className="top-btn"
          data-testid="pos-more"
          aria-haspopup="menu"
          aria-expanded={moreOpen}
          onClick={() => setMoreOpen(true)}
        >
          <MoreHorizontal size={22} aria-hidden />
          <span className="top-label">{t("More")}</span>
        </button>
      </header>
      <div className="pos-body">
        <main className="pos-main">
          <div className="pos-toolbar">
            <div className="scan-box">
              <ScanBarcode size={22} className="scan-icon" aria-hidden />
              <input
                ref={scanRef}
                className="input"
                placeholder={t("Scan or search")}
                aria-label={t("Scan barcode or search product")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={onScanKey}
                autoComplete="off"
                spellCheck={false}
                data-testid="scan-input"
              />
              {query ? (
                <Button
                  variant="ghost"
                  className="clear"
                  aria-label={t("Clear search")}
                  icon={<X size={20} />}
                  onClick={() => (setQuery(""), focusScan())}
                />
              ) : null}
              {results ? (
                <div className="results scan-results" role="listbox" aria-label={t("Search results")}>
                  {results.length === 0 ? (
                    <div className="empty">
                      <h3>{t("No products found")}</h3>
                      <p>{t("Check the spelling or scan the barcode.")}</p>
                    </div>
                  ) : (
                    results.map((r, i) => (
                      <div
                        key={r.product_id}
                        role="option"
                        aria-selected={i === sel}
                        className={`result-row ${i === sel ? "sel" : ""}`}
                        onMouseDown={(e) => (e.preventDefault(), void addProduct(r))}
                      >
                        <ProductImage hash={r.image_hash} name={r.name} size="sm" />
                        <div className="grow">
                          <div className="r-name ellipsis">{r.name}</div>
                          <div className="tiny ellipsis">
                            {r.primary_barcode ?? r.sku}
                            {r.track_inventory ? ` · ${t("Stock {0}", formatQty(r.stock_milli))}` : ""}
                          </div>
                        </div>
                        <div className="r-price money">
                          {r.price_minor === null ? t("No price") : formatMoney(r.price_minor)}
                        </div>
                      </div>
                    ))
                  )}
                </div>
              ) : null}
            </div>
            <div className="quick-actions narrow-only">
              <button type="button" className="q-btn" onClick={() => setModal({ kind: "customer" })}>
                <Users size={20} aria-hidden />
                <span>{t("Customer")}</span>
                <kbd>F3</kbd>
              </button>
              <button
                type="button"
                className="q-btn"
                onClick={() => setModal({ kind: "hold" })}
                disabled={!hasLines || !has("pos.hold")}
              >
                <PauseCircle size={20} aria-hidden />
                <span>{t("Hold")}</span>
                <kbd>F4</kbd>
              </button>
              <button
                type="button"
                className="q-btn"
                onClick={() => setModal({ kind: "discount", lineId: null })}
                disabled={!hasLines}
              >
                <Percent size={20} aria-hidden />
                <span>{t("Sale discount")}</span>
              </button>
              <button type="button" className="q-btn" onClick={() => setModal({ kind: "refund" })}>
                <RotateCcw size={20} aria-hidden />
                <span>{t("Refund")}</span>
              </button>
            </div>
          </div>
          {notice ? (
            <div className={`pos-notice ${notice.tone}`} role={notice.tone === "info" ? "status" : "alert"}>
              <span className="grow">{notice.text}</span>
              <button type="button" className="notice-x" aria-label={t("Dismiss")} onClick={() => setNotice(null)}>
                <X size={18} aria-hidden />
              </button>
            </div>
          ) : null}
          {printFailed && modal.kind === "none" ? (
            <div className="pos-notice warning" role="alert">
              <span className="grow">
                {t("Sale {0} completed — receipt could not be printed", lastSale!.receipt_number)}
              </span>
              <Button
                size="sm"
                onClick={async () => {
                  const r = await api.print.retry(lastSale!.print!.job_id!);
                  setLastSale({ ...lastSale!, print: r });
                  if (r.status === "printed") toast("success", t("Receipt printed"));
                }}
              >
                {t("Retry Print")}
              </Button>
            </div>
          ) : null}
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
              onRedeem={cart.loyalty ? () => setModal({ kind: "redeem" }) : undefined}
              onCustomer={() => setModal({ kind: "customer" })}
            />
          </section>
        </main>
        <aside className="pos-checkout" aria-label={t("Totals")}>
          <button
            type="button"
            className={`co-customer ${cart.customer ? "has" : ""}`}
            onClick={() => setModal({ kind: "customer" })}
            data-testid="checkout-customer"
          >
            <span className="co-avatar" aria-hidden>
              {cart.customer ? initials(cart.customer.name) : <UserPlus size={20} />}
            </span>
            <span className="co-who">
              <span className="co-name ellipsis" dir="auto">
                {cart.customer ? cart.customer.name : t("Add customer")}
              </span>
              <span className="co-sub ellipsis" dir="auto">
                {cart.customer
                  ? [cart.customer.phone, cart.customer.area].filter(Boolean).join(" · ") || t("Customer")
                  : t("For loyalty, credit or Send")}
              </span>
            </span>
            <kbd>F3</kbd>
          </button>
          <div className="co-actions">
            <button
              type="button"
              className="co-tile"
              onClick={() => setModal({ kind: "hold" })}
              disabled={!hasLines || !has("pos.hold")}
            >
              <PauseCircle size={22} aria-hidden />
              <span>{t("Hold")}</span>
            </button>
            <button
              type="button"
              className="co-tile"
              onClick={() => setModal({ kind: "discount", lineId: null })}
              disabled={!hasLines}
            >
              <Percent size={22} aria-hidden />
              <span>{t("Sale discount")}</span>
            </button>
            <button type="button" className="co-tile" onClick={() => setModal({ kind: "refund" })}>
              <RotateCcw size={22} aria-hidden />
              <span>{t("Refund")}</span>
            </button>
          </div>
          <span className="co-spacer">
            {lastSale && !hasLines ? (
              <div className="co-last" data-testid="last-sale">
                <div className="eyebrow">{t("Last sale")}</div>
                <div className="co-last-row">
                  <span className="num strong">{lastSale.receipt_number}</span>
                  <span className="money strong">{formatMoney(lastSale.total_minor)}</span>
                </div>
                {lastSale.change_minor > 0 ? (
                  <div className="co-last-row tiny">
                    <span>{t("Change given")}</span>
                    <span className="money">{formatMoney(lastSale.change_minor)}</span>
                  </div>
                ) : null}
                {has("pos.reprint") ? (
                  <Button
                    size="sm"
                    icon={<Printer size={16} />}
                    onClick={async () => {
                      try {
                        const r = await api.sales.reprint(lastSale.sale_id);
                        toast(
                          r.status === "printed" ? "success" : "warning",
                          r.status === "printed" ? t("Receipt reprinted") : t("Receipt not printed"),
                        );
                      } catch (e) {
                        const ex = explain(e);
                        toast("error", ex.message);
                      }
                    }}
                  >
                    {t("Reprint")}
                  </Button>
                ) : null}
              </div>
            ) : null}
          </span>
          <dl className="dock-sums">
            <div>
              <dt>{t("Subtotal")}</dt>
              <dd className="money">{formatMoney(cart.totals.subtotal_minor)}</dd>
            </div>
            <div className="opt">
              <dt>{t("Discount")}</dt>
              <dd className="money">
                {cart.totals.discount_minor ? formatMoney(-cart.totals.discount_minor) : formatMoney(0)}
              </dd>
            </div>
            <div>
              <dt>
                {t("VAT")} {cart.lines.some((l) => l.tax_inclusive) ? t("(included)") : ""}
              </dt>
              <dd className="money">{formatMoney(cart.totals.tax_minor)}</dd>
            </div>
            <div className="opt">
              <dt>{t("Items")}</dt>
              <dd className="num">{formatQty(cart.totals.item_count_milli)}</dd>
            </div>
          </dl>
          <div className="dock-total">
            <span className="label">{t("TOTAL")}</span>
            <span className="amount money" data-testid="cart-total">
              {formatMoney(cart.totals.total_minor)}
            </span>
          </div>
          <Button
            variant="pay"
            className="pay-btn"
            onClick={() => openPay(tenders[0]?.method ?? "cash")}
            disabled={!hasLines}
            data-testid="pay"
            aria-label={t("PAY {0}", formatMoney(cart.totals.total_minor))}
          >
            <span className="pay-word">{t("PAY")}</span>
            <kbd>F9</kbd>
          </Button>
        </aside>
      </div>
      {moreOpen ? (
        <Modal title={t("More")} size="sheet narrow" onClose={() => (setMoreOpen(false), focusScan())}>
          <div className="more-grid" role="menu" aria-label={t("More")}>
            {moreItems
              .filter((i) => i.show)
              .map((i) => (
                <button
                  key={i.label}
                  type="button"
                  role="menuitem"
                  className="more-row"
                  onClick={() => {
                    setMoreOpen(false);
                    i.run();
                  }}
                >
                  {i.icon}
                  <span>{i.label}</span>
                </button>
              ))}
          </div>
        </Modal>
      ) : null}
      {selected && selected.stock_milli !== null && selected.stock_milli <= 0 ? (
        <span className="sr-only">{t("{0}: recorded stock {1}", selected.name, formatQty(selected.stock_milli))}</span>
      ) : null}

      {modal.kind === "pay" && cart.cart_id ? (
        <PaymentModal
          cart={cart}
          initialMethod={modal.method}
          tenders={tenders}
          onClose={closeModal}
          onPaid={afterSale}
          onCartChanged={applyCart}
        />
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
              toast(
                r.status === "printed" ? "success" : "warning",
                r.status === "printed" ? t("Receipt reprinted") : t("Receipt not printed"),
                r.message ?? undefined,
              );
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
          onHeld={(ticket) => {
            setCart(EMPTY_CART);
            setSelectedLine(null);
            toast("success", ticket ? t("Sale held as ticket #{0}", ticket) : t("Sale held"));
            closeModal();
            void reloadHeld();
          }}
        />
      ) : null}
      {modal.kind === "held" ? (
        <HeldCartsDialog
          onClose={closeModal}
          onRestored={(c) => {
            applyCart(c);
            // Prices changed while the sale was held: the cashier must acknowledge
            // the recalculated basket before continuing (spec 9.2).
            if (c.notices.length) setModal({ kind: "price_changes", notices: c.notices });
            else closeModal();
          }}
          currentHasLines={hasLines}
        />
      ) : null}
      {modal.kind === "price_changes" ? (
        <Modal
          title={t("Prices changed since this sale was held")}
          size="sm"
          onClose={closeModal}
          footer={
            <Button variant="primary" className="right" autoFocus onClick={closeModal} data-testid="ack-price-changes">
              {t("Continue with current prices")}
            </Button>
          }
        >
          <div className="col gap-8">
            <Banner tone="warning">
              {t("The basket was recalculated with today's prices. Tell the customer before taking payment.")}
            </Banner>
            <ul className="small" style={{ paddingInlineStart: 18 }}>
              {modal.notices.map((n) => (
                <li key={n}>{tb(n)}</li>
              ))}
            </ul>
            <div className="row">
              <span className="muted">{t("New total")}</span>
              <strong className="right money">{formatMoney(cart.totals.total_minor)}</strong>
            </div>
          </div>
        </Modal>
      ) : null}
      {modal.kind === "redeem" && cart.loyalty ? (
        <LoyaltyRedeemDialog
          cart={cart}
          onClose={closeModal}
          onDone={(c) => {
            applyCart(c);
            closeModal();
          }}
        />
      ) : null}
      {modal.kind === "orders" ? (
        <Modal title={t("Orders")} size="xl" onClose={closeModal}>
          {hasLines ? (
            <Banner tone="info">{t("Hold or finish the current sale before selling an order.")}</Banner>
          ) : null}
          <OrdersList
            onConverted={(c) => {
              applyCart(c);
              closeModal();
              toast("success", t("Order loaded. Take payment as usual."));
            }}
          />
        </Modal>
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
        <QtyDialog
          line={cart.lines.find((l) => l.line_id === modal.lineId)!}
          onClose={closeModal}
          onApply={(q) => (closeModal(), lineAction((tok) => api.pos.setQty(modal.lineId, q, tok)))}
        />
      ) : null}
      {modal.kind === "discount" ? (
        <DiscountDialog
          line={modal.lineId ? (cart.lines.find((l) => l.line_id === modal.lineId) ?? null) : null}
          cart={cart}
          maxBp={config?.pos.cashier_max_discount_bp ?? 1000}
          onClose={closeModal}
          onApply={(minor, bp) => {
            closeModal();
            const lineId = modal.lineId;
            void lineAction((tok) =>
              lineId ? api.pos.lineDiscount(lineId, minor, bp, tok) : api.pos.cartDiscount(minor, bp, tok),
            );
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
      {modal.kind === "cash" ? (
        <CashEventDialog
          kind={modal.cashKind}
          safeDropTotal={shift.safe_drop_minor}
          onClose={closeModal}
          onDone={() => (closeModal(), void reloadShift())}
        />
      ) : null}
      {modal.kind === "refund" ? <RefundFlow onClose={closeModal} onDone={() => void reloadShift()} /> : null}
      {modal.kind === "recent" ? <RecentSalesDialog onClose={closeModal} /> : null}
      {modal.kind === "print_queue" ? <PrintQueueDialog onClose={closeModal} /> : null}
      {modal.kind === "delivery" ? (
        <DeliveryQuickDialog saleId={modal.saleId} customer={cart.customer} onClose={closeModal} />
      ) : null}
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
      {railOpen ? (
        <SendRail
          reloadKey={railKey}
          onClose={() => (setRailOpen(false), focusScan())}
          onOpen={(r) => setTicketId(r.ticket_id)}
        />
      ) : null}
      {ticketId ? (
        <TicketSheet
          ticketId={ticketId}
          onClose={() => (setTicketId(null), focusScan())}
          onChanged={() => void reloadSends()}
          onRungUp={
            hasLines
              ? undefined
              : (c) => {
                  applyCart(c);
                  setTicketId(null);
                  setRailOpen(false);
                  toast("success", t("Order loaded. Take payment as usual."));
                  void reloadSends();
                }
          }
        />
      ) : null}
      {aiOpen ? (
        <aside className="till-ai" role="complementary" aria-label={t("AI Assistant")} data-testid="till-ai-drawer">
          <div className="till-ai-head">
            <Bot size={20} aria-hidden />
            <h2 className="grow">{t("AI Assistant")}</h2>
            <Button
              variant="ghost"
              className="close-btn"
              aria-label={t("Close")}
              icon={<X size={22} />}
              onClick={() => (setAiOpen(false), focusScan())}
            />
          </div>
          {/* The till has no router; links in answers set the admin page shown on switching to Admin. */}
          <HashRouter>
            <AiReady>{(st) => <AiChat status={st} compact context={cartContext} />}</AiReady>
          </HashRouter>
        </aside>
      ) : null}
      <span className="sr-only" aria-live="polite">
        {t("{0} items, total {1}", cart.lines.length, formatMoney(cart.totals.total_minor))}
      </span>
    </div>
  );
}
