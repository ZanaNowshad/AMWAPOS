import { useEffect, useRef, useState, type ComponentType } from "react";
import { HashRouter, NavLink, Navigate, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import {
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
} from "lucide-react";
import { useSession } from "../../state/session";
import { initials } from "../login/Login";
import { Logo } from "../../components/Logo";
import { ConnectionPill } from "../pos/ConnectionPill";
import { Denied } from "./common";
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
import { WhatsAppPage, AiAssistantPage, InvoiceScanPage, PaymentReviewsPage } from "./automation";

interface NavItem {
  path: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  perm?: string | string[];
  element: ComponentType;
}

const NAV: { group: string; items: NavItem[] }[] = [
  {
    group: "OVERVIEW",
    items: [
      {
        path: "dashboard",
        label: "Dashboard",
        icon: LayoutDashboard,
        perm: ["reports.sales", "admin.access"],
        element: Dashboard,
      },
    ],
  },
  {
    group: "SALES",
    items: [
      { path: "sales", label: "Sales", icon: ShoppingCart, perm: "sales.view", element: SalesPage },
      { path: "refunds", label: "Refunds", icon: RefreshCcw, perm: "sales.view", element: RefundsPage },
      { path: "shifts", label: "Shifts", icon: ClipboardList, perm: "sales.view", element: ShiftsPage },
      { path: "cash", label: "Cash", icon: Wallet, perm: "sales.view", element: CashEventsPage },
    ],
  },
  {
    group: "CATALOG",
    items: [
      { path: "products", label: "Products", icon: Boxes, perm: "products.view", element: ProductsPage },
      { path: "categories", label: "Categories", icon: Layers, perm: "products.view", element: CategoriesPage },
      { path: "pricing", label: "Pricing", icon: Tags, perm: "prices.manage", element: PricingPage },
      {
        path: "unknown-barcodes",
        label: "Unknown Barcodes",
        icon: Barcode,
        perm: ["barcodes.resolve", "products.manage"],
        element: UnknownBarcodesPage,
      },
    ],
  },
  {
    group: "INVENTORY",
    items: [
      { path: "inventory", label: "Inventory", icon: Warehouse, perm: "inventory.view", element: InventoryPage },
      { path: "movements", label: "Stock Movements", icon: History, perm: "inventory.view", element: MovementsPage },
      {
        path: "stocktake",
        label: "Stocktake",
        icon: ClipboardCheck,
        perm: "stocktake.manage",
        element: StocktakesPage,
      },
    ],
  },
  {
    group: "PURCHASING",
    items: [
      { path: "suppliers", label: "Suppliers", icon: Building2, perm: "suppliers.manage", element: SuppliersPage },
      {
        path: "purchase-orders",
        label: "Purchase Orders",
        icon: FileText,
        perm: ["purchasing.manage", "inventory.receive"],
        element: PurchaseOrdersPage,
      },
      { path: "receiving", label: "Receiving", icon: PackageCheck, perm: "inventory.receive", element: ReceivingPage },
      {
        path: "invoice-scan",
        label: "Invoice Scan",
        icon: FileScan,
        perm: "purchasing.manage",
        element: InvoiceScanPage,
      },
    ],
  },
  {
    group: "CUSTOMERS",
    items: [
      { path: "customers", label: "Customers", icon: Users, perm: "customers.view", element: CustomersPage },
      { path: "deliveries", label: "Deliveries", icon: Truck, perm: "deliveries.view", element: DeliveriesPage },
    ],
  },
  {
    group: "BUSINESS",
    items: [
      {
        path: "reports",
        label: "Reports",
        icon: ScrollText,
        perm: ["reports.sales", "reports.financial", "reports.tax", "inventory.view"],
        element: ReportsHome,
      },
      { path: "analytics", label: "Analytics", icon: LineChart, perm: "reports.financial", element: AnalyticsPage },
    ],
  },
  {
    group: "AUTOMATION",
    items: [
      { path: "whatsapp", label: "WhatsApp", icon: MessageCircle, perm: "whatsapp.manage", element: WhatsAppPage },
      {
        path: "payment-reviews",
        label: "Payment Reviews",
        icon: BadgeCheck,
        perm: "whatsapp.manage",
        element: PaymentReviewsPage,
      },
      { path: "ai", label: "AI Assistant", icon: Bot, perm: "ai.use", element: AiAssistantPage },
    ],
  },
  {
    group: "SYSTEM",
    items: [
      { path: "users", label: "Users & Roles", icon: ShieldCheck, perm: "users.manage", element: UsersPage },
      {
        path: "devices",
        label: "Devices",
        icon: Monitor,
        perm: ["devices.manage", "diagnostics.view"],
        element: DevicesPage,
      },
      { path: "sync", label: "Sync / Hub", icon: Cloud, perm: ["sync.manage", "devices.manage"], element: SyncPage },
      { path: "import", label: "Import", icon: FileInput, perm: "import.run", element: ImportPage },
      { path: "backups", label: "Backups", icon: HardDriveDownload, perm: "backup.manage", element: BackupsPage },
      { path: "audit", label: "Audit", icon: Activity, perm: "audit.view", element: AuditPage },
      { path: "settings", label: "Settings", icon: Settings, perm: "settings.manage", element: SettingsPage },
      {
        path: "diagnostics",
        label: "Diagnostics",
        icon: Stethoscope,
        perm: "diagnostics.view",
        element: DiagnosticsPage,
      },
      { path: "updates", label: "Updates", icon: Database, perm: "settings.manage", element: UpdatesPage },
    ],
  },
];

const EXTRA: { path: string; perm?: string | string[]; element: ComponentType }[] = [
  { path: "products/new", perm: "products.manage", element: ProductEditorPage },
  { path: "products/:id", perm: "products.view", element: ProductEditorPage },
  { path: "stocktake/:id", perm: "stocktake.manage", element: StocktakeDetailPage },
  { path: "suppliers/:id", perm: "suppliers.manage", element: SupplierDetailPage },
  { path: "purchase-orders/:id", perm: ["purchasing.manage", "inventory.receive"], element: PoEditorPage },
  { path: "customers/:id", perm: "customers.view", element: CustomerDetailPage },
  { path: "reports/:key", perm: undefined, element: ReportViewer },
  { path: "roles", perm: "users.manage", element: RolesPage },
  { path: "profile", perm: undefined, element: ProfilePage },
];

export function AdminShell() {
  return (
    <HashRouter>
      <Shell />
    </HashRouter>
  );
}

function Shell() {
  const { session, has, setMode, lock, logout, config, status } = useSession();
  const [collapsed, setCollapsed] = useState(() => window.innerWidth < 1200);
  const [menu, setMenu] = useState(false);
  const loc = useLocation();
  const nav = useNavigate();
  const menuRef = useRef<HTMLDivElement>(null);
  const allowed = (perm?: string | string[]) => !perm || (Array.isArray(perm) ? perm.some(has) : has(perm));
  const current = NAV.flatMap((g) => g.items).find((i) => loc.pathname.startsWith("/admin/" + i.path));
  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenu(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, []);
  const firstAllowed = NAV.flatMap((g) => g.items).find((i) => allowed(i.perm));
  return (
    <div className={`admin ${collapsed ? "collapsed" : ""}`} data-testid="admin">
      <aside className="sidebar">
        <div className="sb-brand">
          <Logo size={28} />
          <span className="sb-label">AMWAPOS</span>
        </div>
        <nav aria-label="Admin navigation">
          {NAV.map((g) => {
            const items = g.items.filter((i) => allowed(i.perm));
            if (!items.length) return null;
            return (
              <div key={g.group}>
                <div className="sb-group">{g.group}</div>
                {items.map((i) => (
                  <NavLink
                    key={i.path}
                    to={`/admin/${i.path}`}
                    className={({ isActive }) => `sb-link ${isActive ? "active" : ""}`}
                    title={i.label}
                  >
                    <i.icon size={17} />
                    <span className="sb-label">{i.label}</span>
                  </NavLink>
                ))}
              </div>
            );
          })}
        </nav>
      </aside>
      <div className="admin-main">
        <header className="topbar">
          <button className="btn ghost icon" aria-label="Toggle navigation" onClick={() => setCollapsed((c) => !c)}>
            <Menu size={18} />
          </button>
          <div className="tiny">
            Admin / <strong style={{ color: "var(--text)" }}>{current?.label ?? "…"}</strong>
          </div>
          <div className="grow" />
          <span className="small muted row">
            <Store size={15} /> {config?.business_name} · {status.device?.name}
          </span>
          <span style={{ filter: "invert(0)" }}>
            <ConnectionPill />
          </span>
          <button className="btn ghost icon" aria-label="Alerts" onClick={() => nav("/admin/dashboard")}>
            <Bell size={18} />
          </button>
          <div style={{ position: "relative" }} ref={menuRef}>
            <button className="btn ghost" onClick={() => setMenu((m) => !m)} aria-haspopup="menu" aria-expanded={menu}>
              <span className="avatar sm">{initials(session?.display_name ?? "?")}</span>
              {session?.display_name}
            </button>
            {menu ? (
              <div className="menu" role="menu">
                <button role="menuitem" onClick={() => (setMenu(false), nav("/admin/profile"))}>
                  <UserRound size={15} /> My Profile
                </button>
                {has("pos.sell") ? (
                  <button role="menuitem" onClick={() => setMode("cashier")}>
                    <Receipt size={15} /> Switch to POS
                  </button>
                ) : null}
                <button role="menuitem" onClick={() => void lock()}>
                  <Lock size={15} /> Lock
                </button>
                <button role="menuitem" onClick={() => void logout()}>
                  <LogOut size={15} /> Logout
                </button>
              </div>
            ) : null}
          </div>
          {has("pos.sell") ? (
            <button className="btn primary sm" onClick={() => setMode("cashier")} title="Return to checkout">
              <Gauge size={15} /> POS
            </button>
          ) : null}
        </header>
        <main className="content" id="admin-content">
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
