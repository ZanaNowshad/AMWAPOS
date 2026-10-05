import { useEffect, useRef, useState, type ComponentType } from "react";
import { HashRouter, NavLink, Navigate, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import {
  Search,
  Activity,
  Barcode,
  Bell,
  Boxes,
  Bot,
  Building2,
  ClipboardCheck,
  ClipboardList,
  Cloud,
  Database,
  FileInput,
  FolderInput,
  FileScan,
  FileText,
  Gauge,
  HardDriveDownload,
  History,
  Layers,
  LayoutDashboard,
  Lock,
  LogOut,
  Menu,
  MessageCircle,
  Monitor,
  PackageCheck,
  Receipt,
  RefreshCcw,
  ScrollText,
  Settings,
  ShieldCheck,
  ShoppingCart,
  Stethoscope,
  Store,
  Tags,
  Truck,
  UserRound,
  Users,
  Wallet,
  Warehouse,
  LineChart,
  BadgeCheck,
  ArrowLeftRight,
  CalendarCheck,
  Smartphone,
  ShoppingBag,
  Landmark,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import type { FeatureName } from "../../api/types";
import { BranchesPage, BranchSwitcher, EndOfDayPage, OrdersPage, PhoneViewPage, TransfersPage } from "./pillars";
import { useSession } from "../../state/session";
import { initials } from "../login/Login";
import { Logo } from "../../components/Logo";
import { ConnectionPill } from "../pos/ConnectionPill";
import { Denied } from "./common";
import { Modal } from "../../components/ui";
import { Dashboard } from "./Dashboard";
import { SalesPage, RefundsPage, ShiftsPage, CashEventsPage } from "./sales";
import { ProductsPage, ProductEditorPage, CategoriesPage, PricingPage, UnknownBarcodesPage } from "./catalog";
import { InventoryPage, MovementsPage, StocktakesPage, StocktakeDetailPage, ReceivingPage } from "./inventory";
import { SuppliersPage, SupplierDetailPage, PurchaseOrdersPage, PoEditorPage } from "./purchasing";
import { CustomersPage, CustomerDetailPage, DeliveriesPage } from "./customers";
import { ReportsHome, ReportViewer, AnalyticsPage } from "./reports";
import { UsersPage, RolesPage, ProfilePage } from "./staff";
import {
  DevicesPage,
  SyncPage,
  ImportPage,
  BackupsPage,
  AuditPage,
  SettingsPage,
  DiagnosticsPage,
  UpdatesPage,
} from "./system";
import { WhatsAppPage, InvoiceScanPage, PaymentReviewsPage } from "./automation";
import { DocumentReviewPage } from "./documents";
import { WhatsAppOrdersPage } from "./waOrders";
import { AiAssistantPage } from "./aiChat";
import { MigrationPage } from "./migration";
import { PayablesPage } from "./payables";
import { ExpensesPage } from "./expenses";
import { t } from "../../i18n";
import { LanguageToggle } from "../../components/LanguageToggle";
import { BackupAlert } from "../../components/BackupAlert";

interface NavItem {
  path: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  perm?: string | string[];
  element: ComponentType;
  /** Optional module: hidden from the menu while its feature flag is off. */
  feature?: FeatureName;
  /** Rarely needed: listed under "More tools" in its group until opened. */
  advanced?: boolean;
}

const NAV: { group: string; items: NavItem[] }[] = [
  {
    group: t("OVERVIEW"),
    items: [
      {
        path: "dashboard",
        label: t("Dashboard"),
        icon: LayoutDashboard,
        perm: ["reports.sales", "admin.access"],
        element: Dashboard,
      },
    ],
  },
  {
    // The order journey, in the order it happens: message → order → delivery → payment.
    group: t("ORDERS & DELIVERY"),
    items: [
      {
        path: "whatsapp-orders",
        label: t("WhatsApp orders"),
        icon: ShoppingBag,
        perm: ["orders.manage", "whatsapp.manage", "whatsapp.send"],
        element: WhatsAppOrdersPage,
        feature: "orders.whatsapp_ai",
      },
      {
        path: "orders",
        label: t("Orders"),
        icon: ClipboardList,
        perm: ["orders.manage", "pos.sell"],
        element: OrdersPage,
        feature: "orders.digital",
      },
      { path: "deliveries", label: t("Deliveries"), icon: Truck, perm: "deliveries.view", element: DeliveriesPage },
      {
        path: "payment-reviews",
        label: t("Payment checks"),
        icon: BadgeCheck,
        perm: "whatsapp.manage",
        element: PaymentReviewsPage,
        feature: "ocr.payment_screenshots",
      },
      { path: "customers", label: t("Customers"), icon: Users, perm: "customers.view", element: CustomersPage },
    ],
  },
  {
    group: t("SALES"),
    items: [
      { path: "sales", label: t("Sales"), icon: ShoppingCart, perm: "sales.view", element: SalesPage },
      { path: "refunds", label: t("Refunds"), icon: RefreshCcw, perm: "sales.view", element: RefundsPage },
      { path: "shifts", label: t("Shifts"), icon: ClipboardList, perm: "sales.view", element: ShiftsPage },
      { path: "cash", label: t("Cash"), icon: Wallet, perm: "sales.view", element: CashEventsPage },
    ],
  },
  {
    group: t("CATALOG"),
    items: [
      { path: "products", label: t("Products"), icon: Boxes, perm: "products.view", element: ProductsPage },
      { path: "categories", label: t("Categories"), icon: Layers, perm: "products.view", element: CategoriesPage },
      { path: "pricing", label: t("Pricing"), icon: Tags, perm: "prices.manage", element: PricingPage },
      {
        path: "unknown-barcodes",
        label: t("Unknown barcodes"),
        icon: Barcode,
        perm: ["barcodes.resolve", "products.manage"],
        element: UnknownBarcodesPage,
      },
    ],
  },
  {
    group: t("INVENTORY"),
    items: [
      { path: "inventory", label: t("Inventory"), icon: Warehouse, perm: "inventory.view", element: InventoryPage },
      { path: "movements", label: t("Stock movements"), icon: History, perm: "inventory.view", element: MovementsPage },
      {
        path: "stocktake",
        label: t("Stocktake"),
        icon: ClipboardCheck,
        perm: "stocktake.manage",
        element: StocktakesPage,
      },
      {
        path: "transfers",
        label: t("Locations & transfers"),
        icon: ArrowLeftRight,
        perm: "inventory.view",
        element: TransfersPage,
        feature: "inventory.locations",
      },
    ],
  },
  {
    group: t("PURCHASING"),
    items: [
      { path: "suppliers", label: t("Suppliers"), icon: Building2, perm: "suppliers.manage", element: SuppliersPage },
      {
        path: "purchase-orders",
        label: t("Purchase orders"),
        icon: FileText,
        perm: ["purchasing.manage", "inventory.receive"],
        element: PurchaseOrdersPage,
      },
      {
        path: "receiving",
        label: t("Receiving"),
        icon: PackageCheck,
        perm: "inventory.receive",
        element: ReceivingPage,
      },
      {
        path: "payables",
        label: t("Payables"),
        icon: Landmark,
        perm: ["payables.view", "purchasing.manage"],
        element: PayablesPage,
      },
      {
        path: "invoice-scan",
        label: t("Supplier documents"),
        icon: FileScan,
        perm: ["ocr.scan", "purchasing.manage"],
        element: InvoiceScanPage,
        feature: "ocr.supplier_invoices",
      },
    ],
  },
  {
    group: t("BUSINESS"),
    items: [
      {
        path: "reports",
        label: t("Reports"),
        icon: ScrollText,
        perm: ["reports.sales", "reports.financial", "reports.tax", "inventory.view"],
        element: ReportsHome,
      },
      { path: "expenses", label: t("Expenses"), icon: Receipt, perm: "expenses.view", element: ExpensesPage },
      { path: "analytics", label: t("Analytics"), icon: LineChart, perm: "reports.financial", element: AnalyticsPage },
      { path: "end-of-day", label: t("End of day"), icon: CalendarCheck, perm: "reports.sales", element: EndOfDayPage },
      {
        path: "phone-view",
        label: t("Phone view"),
        icon: Smartphone,
        perm: "reports.financial",
        element: PhoneViewPage,
        feature: "pwa.companion",
      },
    ],
  },
  {
    group: t("AUTOMATION"),
    items: [
      {
        path: "whatsapp",
        label: t("WhatsApp"),
        icon: MessageCircle,
        perm: "whatsapp.manage",
        element: WhatsAppPage,
        feature: "whatsapp.enabled",
      },
      {
        path: "ai",
        label: t("AI Assistant"),
        icon: Bot,
        perm: "ai.use",
        element: AiAssistantPage,
        feature: "ai.enabled",
      },
    ],
  },
  {
    group: t("SYSTEM"),
    items: [
      { path: "users", label: t("Users & Roles"), icon: ShieldCheck, perm: "users.manage", element: UsersPage },
      {
        path: "branches",
        label: t("Branches"),
        icon: Store,
        perm: ["branches.manage", "branches.all"],
        element: BranchesPage,
        feature: "org.multi_branch",
        advanced: true,
      },
      {
        path: "devices",
        label: t("Devices"),
        icon: Monitor,
        perm: ["devices.manage", "diagnostics.view"],
        element: DevicesPage,
        advanced: true,
      },
      {
        path: "sync",
        label: t("Sync / Hub"),
        icon: Cloud,
        perm: ["sync.manage", "devices.manage"],
        element: SyncPage,
        advanced: true,
      },
      { path: "import", label: t("Import"), icon: FileInput, perm: "import.run", element: ImportPage, advanced: true },
      {
        path: "migration",
        label: t("Migration"),
        icon: FolderInput,
        perm: "import.run",
        element: MigrationPage,
        advanced: true,
      },
      { path: "backups", label: t("Backups"), icon: HardDriveDownload, perm: "backup.manage", element: BackupsPage },
      { path: "audit", label: t("Audit"), icon: Activity, perm: "audit.view", element: AuditPage, advanced: true },
      { path: "settings", label: t("Settings"), icon: Settings, perm: "settings.manage", element: SettingsPage },
      {
        path: "diagnostics",
        label: t("Diagnostics"),
        icon: Stethoscope,
        perm: "diagnostics.view",
        element: DiagnosticsPage,
        advanced: true,
      },
      {
        path: "updates",
        label: t("Updates"),
        icon: Database,
        perm: "settings.manage",
        element: UpdatesPage,
        advanced: true,
      },
    ],
  },
];

const EXTRA: { path: string; perm?: string | string[]; element: ComponentType; label?: string }[] = [
  { path: "products/new", perm: "products.manage", element: ProductEditorPage },
  { path: "products/:id", perm: "products.view", element: ProductEditorPage },
  { path: "stocktake/:id", perm: "stocktake.manage", element: StocktakeDetailPage },
  { path: "suppliers/:id", perm: "suppliers.manage", element: SupplierDetailPage },
  { path: "purchase-orders/:id", perm: ["purchasing.manage", "inventory.receive"], element: PoEditorPage },
  { path: "invoice-scan/:id", perm: ["ocr.scan", "purchasing.manage"], element: DocumentReviewPage },
  { path: "customers/:id", perm: "customers.view", element: CustomerDetailPage },
  { path: "reports/:key", perm: undefined, element: ReportViewer },
  { path: "roles", perm: "users.manage", element: RolesPage, label: t("Roles & Permissions") },
  { path: "profile", perm: undefined, element: ProfilePage, label: t("My profile") },
];

function CommandPalette({
  items,
  onGo,
  onClose,
}: {
  items: { path: string; label: string; group: string; icon: ComponentType<{ size?: number }> }[];
  onGo: (path: string) => void;
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(0);
  const norm = (x: string) => x.toLowerCase().normalize("NFKD");
  const found = items.filter((i) => norm(`${i.label} ${i.group} ${i.path}`).includes(norm(q.trim()))).slice(0, 12);
  return (
    <Modal title={t("Go to…")} onClose={onClose} size="sm" closeOnBackdrop>
      <div className="col gap-8">
        <input
          className="input"
          autoFocus
          value={q}
          placeholder={t("Type a page name")}
          aria-label={t("Search pages")}
          onChange={(e) => (setQ(e.target.value), setSel(0))}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setSel((s) => Math.min(s + 1, found.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setSel((s) => Math.max(s - 1, 0));
            } else if (e.key === "Enter" && found[sel]) onGo(found[sel].path);
          }}
        />
        <div role="listbox" aria-label={t("Pages")}>
          {found.map((i, n) => (
            <button
              key={i.path}
              role="option"
              aria-selected={n === sel}
              className={`list-row ${n === sel ? "active" : ""}`}
              style={{
                display: "flex",
                gap: 10,
                width: "100%",
                padding: "8px 10px",
                textAlign: "start",
                alignItems: "center",
              }}
              onMouseEnter={() => setSel(n)}
              onClick={() => onGo(i.path)}
            >
              <i.icon size={16} />
              <span className="grow">{i.label}</span>
              <span className="tiny">{i.group}</span>
            </button>
          ))}
          {!found.length ? <div className="empty">{t("No matching page.")}</div> : null}
        </div>
      </div>
    </Modal>
  );
}

export function AdminShell() {
  return (
    <HashRouter>
      <Shell />
    </HashRouter>
  );
}

function Shell() {
  const { session, has, setMode, lock, logout, config, status } = useSession();
  // At 1024 the nav is a 64 px icon rail; the menu button opens the labelled nav as a flyout (tap, not hover).
  const [collapsed, setCollapsed] = useState(() => window.innerWidth < 1200);
  const [fly, setFly] = useState(false);
  const [menu, setMenu] = useState(false);
  const loc = useLocation();
  const nav = useNavigate();
  const menuRef = useRef<HTMLDivElement>(null);
  const features = config?.features;
  const allowed = (perm?: string | string[]) => !perm || (Array.isArray(perm) ? perm.some(has) : has(perm));
  const visible = (i: NavItem) => allowed(i.perm) && (!i.feature || !!features?.[i.feature]);
  // The longest match wins ("whatsapp-orders" over "whatsapp").
  const current = NAV.flatMap((g) => g.items)
    .filter((i) => loc.pathname === "/admin/" + i.path || loc.pathname.startsWith("/admin/" + i.path + "/"))
    .sort((a, b) => b.path.length - a.path.length)[0];
  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenu(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, []);
  const firstAllowed = NAV.flatMap((g) => g.items).find((i) => allowed(i.perm));
  // "More tools" stays as the person left it on this computer.
  const [moreOpen, setMoreOpen] = useState(() => {
    try {
      return localStorage.getItem("amwapos.nav.more") === "1";
    } catch {
      return false;
    }
  });
  const toggleMore = (open: boolean) => {
    setMoreOpen(open);
    try {
      localStorage.setItem("amwapos.nav.more", open ? "1" : "0");
    } catch {
      /* storage unavailable: the choice lasts for this session */
    }
  };
  const [palette, setPalette] = useState(false);
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
      }
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);
  return (
    <div className={`admin ${collapsed ? "collapsed" : ""} ${fly ? "flyout" : ""}`} data-testid="admin">
      {fly ? <div className="fly-backdrop" onClick={() => setFly(false)} aria-hidden /> : null}
      <aside className="sidebar">
        <div className="sb-brand">
          <Logo size={28} />
          <span className="sb-label">{t("AMWAPOS")}</span>
        </div>
        <nav aria-label={t("Admin navigation")}>
          {NAV.map((g) => {
            const items = g.items.filter(visible);
            if (!items.length) return null;
            const link = (i: NavItem) => (
              <NavLink
                key={i.path}
                to={`/admin/${i.path}`}
                className={({ isActive }) => `sb-link ${isActive ? "active" : ""}`}
                aria-label={i.label}
                onClick={() => setFly(false)}
              >
                <i.icon size={20} aria-hidden />
                <span className="sb-label">{i.label}</span>
              </NavLink>
            );
            const basic = items.filter((i) => !i.advanced);
            const extra = items.filter((i) => i.advanced);
            // Open while the current page is one of them, or once someone opened it.
            const showExtra = moreOpen || extra.some((i) => i === current);
            return (
              <div key={g.group}>
                <div className="sb-group">{g.group}</div>
                {basic.map(link)}
                {extra.length ? (
                  <>
                    <button
                      type="button"
                      className="sb-link sb-more"
                      aria-expanded={showExtra}
                      aria-label={showExtra ? t("Fewer tools") : t("More tools")}
                      onClick={() => toggleMore(!showExtra)}
                      data-testid="nav-more"
                    >
                      {showExtra ? <ChevronUp size={20} aria-hidden /> : <ChevronDown size={20} aria-hidden />}
                      <span className="sb-label">{showExtra ? t("Fewer tools") : t("More tools")}</span>
                    </button>
                    {showExtra ? extra.map(link) : null}
                  </>
                ) : null}
              </div>
            );
          })}
        </nav>
      </aside>
      <div className="admin-main">
        <header className="topbar">
          <button
            className="btn ghost icon"
            aria-label={t("Toggle navigation")}
            aria-expanded={collapsed ? fly : true}
            onClick={() => (window.innerWidth < 1200 ? setFly((f) => !f) : setCollapsed((c) => !c))}
          >
            <Menu size={22} />
          </button>
          <div className="crumb ellipsis">
            <span className="muted">{t("Admin /")}</span>{" "}
            <strong>{current?.label ?? EXTRA.find((x) => loc.pathname === `/admin/${x.path}`)?.label ?? "…"}</strong>
          </div>
          <div className="grow" />
          <button className="btn ghost" onClick={() => setPalette(true)} aria-label={t("Go to… (Ctrl+K)")}>
            <Search size={20} /> <span className="kbd">Ctrl K</span>
          </button>
          <span className="small muted row store-name">
            <Store size={16} /> {config?.business_name} · {status.device?.name}
          </span>
          <span className="admin-pill">
            <ConnectionPill />
          </span>
          <BranchSwitcher />
          <LanguageToggle />
          <button className="btn ghost icon" aria-label={t("Alerts")} onClick={() => nav("/admin/dashboard")}>
            <Bell size={20} />
          </button>
          <div style={{ position: "relative" }} ref={menuRef}>
            <button
              className="btn ghost user-btn"
              onClick={() => setMenu((m) => !m)}
              aria-haspopup="menu"
              aria-expanded={menu}
              aria-label={session?.display_name}
            >
              <span className="avatar sm">{initials(session?.display_name ?? "?")}</span>
              <span className="user-name ellipsis">{session?.display_name}</span>
            </button>
            {menu ? (
              <div className="menu" role="menu">
                <button role="menuitem" onClick={() => (setMenu(false), nav("/admin/profile"))}>
                  <UserRound size={20} /> {t("My Profile")}
                </button>
                {has("pos.sell") ? (
                  <button role="menuitem" onClick={() => setMode("cashier")}>
                    <Receipt size={20} /> {t("Switch to POS")}
                  </button>
                ) : null}
                <button role="menuitem" onClick={() => void lock()}>
                  <Lock size={20} /> {t("Lock")}
                </button>
                <button role="menuitem" onClick={() => void logout()}>
                  <LogOut size={20} /> {t("Log out")}
                </button>
              </div>
            ) : null}
          </div>
          {has("pos.sell") ? (
            <button
              className="btn primary"
              onClick={() => setMode("cashier")}
              aria-label={t("Return to checkout")}
              data-testid="back-to-pos"
            >
              <Gauge size={20} /> {t("POS")}
            </button>
          ) : null}
        </header>
        {palette ? (
          <CommandPalette
            items={NAV.flatMap((g) =>
              g.items.filter(visible).map((i) => ({ path: i.path, label: i.label, group: g.group, icon: i.icon })),
            )}
            onClose={() => setPalette(false)}
            onGo={(p) => (setPalette(false), nav(`/admin/${p}`))}
          />
        ) : null}
        <main className="content" id="admin-content">
          <BackupAlert />
          <Routes>
            {NAV.flatMap((g) => g.items).map((i) => (
              <Route key={i.path} path={`/admin/${i.path}`} element={allowed(i.perm) ? <i.element /> : <Denied />} />
            ))}
            {EXTRA.map((i) => (
              <Route key={i.path} path={`/admin/${i.path}`} element={allowed(i.perm) ? <i.element /> : <Denied />} />
            ))}
            <Route
              path="*"
              element={firstAllowed ? <Navigate to={`/admin/${firstAllowed.path}`} replace /> : <Denied />}
            />
          </Routes>
        </main>
      </div>
    </div>
  );
}
