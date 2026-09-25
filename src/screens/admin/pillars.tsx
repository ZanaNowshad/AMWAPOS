// Admin screens for the optional modules: digital orders, stock locations and
// transfers, branches, end-of-day pack, owner phone view and loyalty.
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Download, Plus, Star, Truck, PackageCheck, X, Bot } from "lucide-react";
import { api } from "../../api";
import type {
  Branch,
  BranchPrice,
  CompanionToken,
  EodPack,
  LoyaltyCustomer,
  LoyaltySettings,
  Report,
  ReportColumn,
  ReportPreset,
  StockLocation,
  Transfer,
  UserRow,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate, useFeature } from "../../components/FeatureGate";
import {
  Banner,
  Button,
  Checkbox,
  Chip,
  Empty,
  Field,
  Modal,
  PageHeader,
  Skeleton,
  TextInput,
} from "../../components/ui";
import { newOperationId } from "../../lib/ids";
import { formatMoney, formatQty, parseMoney } from "../../lib/money";
import { formatShort, todayLocal } from "../../lib/time";
import { downloadBase64, useAction, useLoad } from "./common";
import { fmtCell } from "./reports";
import { OrdersList } from "../orders";
import { t, tb } from "../../i18n";
import { codeLabel } from "../../i18n/codes";

// ---- Digital orders --------------------------------------------------------

export function OrdersPage() {
  return (
    <div>
      <PageHeader
        title={t("Digital orders")}
        subtitle={t("Phone, WhatsApp and web orders. A person confirms each one; a cashier sells it on a till.")}
      />
      <FeatureGate feature="orders.digital">
        <OrdersList />
      </FeatureGate>
    </div>
  );
}

// ---- Locations and transfers ----------------------------------------------

export function TransfersPage() {
  return (
    <div>
      <PageHeader
        title={t("Locations & transfers")}
        subtitle={t("Move stock between locations and branches: draft, ship, receive. A transfer never creates stock.")}
      />
      <FeatureGate feature="inventory.locations">
        <TransfersBody />
      </FeatureGate>
    </div>
  );
}

function TransfersBody() {
  const { has, session } = useSession();
  const multi = useFeature("org.multi_branch");
  const locs = useLoad(() => api.locations.list(), []);
  const [status, setStatus] = useState("");
  const list = useLoad(() => api.transfers.list(status || undefined), [status]);
  const transit = useLoad(() => api.transfers.inTransit(), []);
  const branches = useLoad(() => (multi ? api.branches.list() : Promise.resolve([] as Branch[])), [multi]);
  const [locEdit, setLocEdit] = useState<StockLocation | "new" | null>(null);
  const [creating, setCreating] = useState(false);
  const [stockOf, setStockOf] = useState<StockLocation | null>(null);
  const act = useAction();
  const [ops] = useState(() => new Map<string, string>());
  const step = async (tr: Transfer, kind: "ship" | "receive") => {
    const key = `${kind}:${tr.transfer_id}`;
    const op = ops.get(key) ?? newOperationId();
    ops.set(key, op);
    const r = await act.run(() =>
      kind === "ship" ? api.transfers.ship(tr.transfer_id, op) : api.transfers.receive(tr.transfer_id, op),
    );
    if (r) {
      await list.reload();
      await transit.reload();
    }
  };
  const mine = (locs.data ?? []).filter((l) => l.branch_id === session?.branch_id);
  return (
    <div className="col gap-24">
      <div className="card card-pad col gap-16">
        <div className="row">
          <h3 className="grow">{t("Locations")}</h3>
          {has("inventory.transfer") ? (
            <Button size="sm" icon={<Plus size={14} />} onClick={() => setLocEdit("new")}>
              {t("Add location")}
            </Button>
          ) : null}
        </div>
        {locs.error ? <Banner tone="danger">{locs.error}</Banner> : null}
        {!locs.data ? <Skeleton rows={2} /> : null}
        <table className="table">
          <tbody>
            {(locs.data ?? []).map((l) => (
              <tr key={l.location_id}>
                <td>
                  <strong>{l.name}</strong> <span className="tiny">{l.code}</span>
                  {l.is_default ? (
                    <>
                      {" "}
                      <Chip tone="brand">{t("Default")}</Chip>
                    </>
                  ) : null}
                  {!l.active ? (
                    <>
                      {" "}
                      <Chip>{t("Inactive")}</Chip>
                    </>
                  ) : null}
                </td>
                {multi ? <td className="small">{l.branch_name}</td> : null}
                <td className="num">
                  <Button size="sm" onClick={() => setStockOf(l)}>
                    {t("Stock")}
                  </Button>{" "}
                  {has("inventory.transfer") && !l.is_default && l.branch_id === session?.branch_id ? (
                    <Button size="sm" onClick={() => setLocEdit(l)}>
                      {t("Edit")}
                    </Button>
                  ) : null}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="tiny">
          {t(
            "The default stockroom holds whatever is not recorded at another location. Selling always uses the branch total.",
          )}
        </div>
      </div>

      {(transit.data ?? []).length ? (
        <div className="card card-pad col gap-8">
          <h3>{t("In transit")}</h3>
          <table className="table">
            <tbody>
              {transit.data!.map((r) => (
                <tr key={`${r.product_id}:${r.to_branch_id}`}>
                  <td>{r.name}</td>
                  <td className="small">{t("to {0}", r.to_branch_name)}</td>
                  <td className="num">{formatQty(r.qty_milli)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}

      <div className="col gap-16">
        <div className="row wrap">
          <h3 className="grow">{t("Transfers")}</h3>
          {["", "draft", "shipped", "received", "cancelled"].map((k) => (
            <button key={k} className={`filter-chip ${status === k ? "active" : ""}`} onClick={() => setStatus(k)}>
              {k ? codeLabel(k) : t("All")}
            </button>
          ))}
          {has("inventory.transfer") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setCreating(true)}>
              {t("New transfer")}
            </Button>
          ) : null}
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        {list.data && list.data.length === 0 ? <Empty title={t("No transfers")} /> : null}
        {(list.data ?? []).map((tr) => (
          <div key={tr.transfer_id} className="card card-pad col gap-8" data-testid="transfer-card">
            <div className="row wrap">
              <strong>{tr.transfer_number}</strong>
              <Chip tone={tr.status === "shipped" ? "warning" : tr.status === "received" ? "success" : "default"}>
                {codeLabel(tr.status)}
              </Chip>
              <span className="small">
                {tr.from_branch_id !== tr.to_branch_id
                  ? `${tr.from_branch_name} › ${tr.to_branch_name}`
                  : `${tr.from_location_name} › ${tr.to_location_name}`}
              </span>
              <span className="grow" />
              <span className="tiny">{formatShort(tr.created_at)}</span>
            </div>
            <div className="small muted">
              {tr.lines.map((l) => `${formatQty(l.qty_milli)} × ${l.product_name}`).join(" · ")}
            </div>
            {tr.note ? <div className="small">{tr.note}</div> : null}
            {has("inventory.transfer") ? (
              <div className="row">
                <span className="grow" />
                {tr.status === "draft" && tr.from_branch_id === session?.branch_id ? (
                  <>
                    <Button
                      size="sm"
                      variant="danger-outline"
                      onClick={() => act.run(() => api.transfers.cancel(tr.transfer_id)).then(() => list.reload())}
                    >
                      {t("Cancel")}
                    </Button>
                    <Button
                      size="sm"
                      variant="primary"
                      icon={<Truck size={14} />}
                      loading={act.busy}
                      onClick={() => void step(tr, "ship")}
                    >
                      {t("Ship")}
                    </Button>
                  </>
                ) : null}
                {tr.status === "shipped" && tr.to_branch_id === session?.branch_id ? (
                  <Button
                    size="sm"
                    variant="primary"
                    icon={<PackageCheck size={14} />}
                    loading={act.busy}
                    onClick={() => void step(tr, "receive")}
                  >
                    {t("Receive")}
                  </Button>
                ) : null}
                {tr.status === "shipped" && tr.to_branch_id !== session?.branch_id ? (
                  <span className="tiny">{t("Received at {0}", tr.to_branch_name)}</span>
                ) : null}
              </div>
            ) : null}
          </div>
        ))}
      </div>

      {locEdit ? (
        <LocationDialog
          loc={locEdit === "new" ? null : locEdit}
          onClose={() => setLocEdit(null)}
          onSaved={(rows) => (locs.setData(rows), setLocEdit(null))}
        />
      ) : null}
      {creating ? (
        <TransferDialog
          locations={mine}
          allLocations={locs.data ?? []}
          branches={(branches.data ?? []).filter((b) => b.active && b.branch_id !== session?.branch_id)}
          onClose={() => setCreating(false)}
          onCreated={() => (setCreating(false), void list.reload())}
        />
      ) : null}
      {stockOf ? <LocationStock loc={stockOf} onClose={() => setStockOf(null)} /> : null}
    </div>
  );
}

function LocationStock({ loc, onClose }: { loc: StockLocation; onClose: () => void }) {
  const { data, error } = useLoad(() => api.locations.stock(loc.location_id), [loc.location_id]);
  return (
    <Modal title={t("Stock at {0}", loc.name)} size="md" onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? <Skeleton /> : null}
      {data && data.length === 0 ? <Empty title={t("Nothing recorded here")} /> : null}
      <table className="table">
        <tbody>
          {(data ?? []).map((r) => (
            <tr key={r.product_id}>
              <td>{r.name}</td>
              <td className="num">{formatQty(r.qty_milli)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Modal>
  );
}

function LocationDialog({
  loc,
  onClose,
  onSaved,
}: {
  loc: StockLocation | null;
  onClose: () => void;
  onSaved: (rows: StockLocation[]) => void;
}) {
  const [code, setCode] = useState(loc?.code ?? "");
  const [name, setName] = useState(loc?.name ?? "");
  const [active, setActive] = useState(loc?.active ?? true);
  const act = useAction();
  return (
    <Modal
      title={loc ? t("Edit location") : t("Add location")}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.locations.save(loc?.location_id ?? null, { code, name, active }));
              if (r) onSaved(r);
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <TextInput
          label={t("Code")}
          value={code}
          onChange={(e) => setCode(e.target.value)}
          hint={t("Short code, e.g. SHELF or COLD")}
        />
        <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} />
        <Checkbox label={t("Active")} checked={active} onChange={setActive} />
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

function TransferDialog({
  locations,
  allLocations,
  branches,
  onClose,
  onCreated,
}: {
  locations: StockLocation[];
  allLocations: StockLocation[];
  branches: Branch[];
  onClose: () => void;
  onCreated: () => void;
}) {
  const [toBranch, setToBranch] = useState("");
  const [from, setFrom] = useState(locations.find((l) => l.is_default)?.location_id ?? "");
  const [to, setTo] = useState("");
  const [note, setNote] = useState("");
  const [lines, setLines] = useState<{ product_id: string; name: string; qty: string }[]>([]);
  const [q, setQ] = useState("");
  const [rows, setRows] = useState<{ product_id: string; name: string; sku: string; stock_milli: number }[]>([]);
  const act = useAction();
  useEffect(() => {
    if (q.trim().length < 2) return setRows([]);
    const h = setTimeout(() => {
      api.products
        .search({ q, limit: 10 })
        .then((p) => setRows(p.rows.filter((r) => r.track_inventory)))
        .catch(() => setRows([]));
    }, 200);
    return () => clearTimeout(h);
  }, [q]);
  const destinations = toBranch
    ? allLocations.filter((l) => l.branch_id === toBranch && l.active)
    : locations.filter((l) => l.active && l.location_id !== from);
  return (
    <Modal
      title={t("New transfer")}
      size="lg"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            disabled={!lines.length || (!toBranch && !to)}
            onClick={async () => {
              const r = await act.run(() =>
                api.transfers.create({
                  to_branch_id: toBranch || null,
                  from_location_id: from || null,
                  to_location_id: to || null,
                  note: note || null,
                  lines: lines.map((l) => ({ product_id: l.product_id, qty_milli: Math.round(Number(l.qty) * 1000) })),
                }),
              );
              if (r) onCreated();
            }}
          >
            {t("Save draft")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="grid-2">
          <Field label={t("From")}>
            <select className="select" value={from} onChange={(e) => setFrom(e.target.value)}>
              {locations
                .filter((l) => l.active)
                .map((l) => (
                  <option key={l.location_id} value={l.location_id}>
                    {l.name}
                  </option>
                ))}
            </select>
          </Field>
          {branches.length ? (
            <Field label={t("To branch")}>
              <select className="select" value={toBranch} onChange={(e) => (setToBranch(e.target.value), setTo(""))}>
                <option value="">{t("This branch")}</option>
                {branches.map((b) => (
                  <option key={b.branch_id} value={b.branch_id}>
                    {b.name}
                  </option>
                ))}
              </select>
            </Field>
          ) : null}
          <Field label={t("To location")}>
            <select className="select" value={to} onChange={(e) => setTo(e.target.value)}>
              <option value="">{toBranch ? t("Their stockroom") : t("Choose…")}</option>
              {destinations.map((l) => (
                <option key={l.location_id} value={l.location_id}>
                  {l.name}
                </option>
              ))}
            </select>
          </Field>
        </div>
        <div className="col gap-8">
          {lines.map((l, i) => (
            <div key={l.product_id} className="row">
              <span className="grow">{l.name}</span>
              <input
                className="input num"
                style={{ width: 100 }}
                aria-label={t("Quantity")}
                value={l.qty}
                onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
              />
              <Button
                size="sm"
                variant="ghost"
                aria-label={t("Remove")}
                icon={<X size={14} />}
                onClick={() => setLines(lines.filter((_, j) => j !== i))}
              />
            </div>
          ))}
          <input className="input" placeholder={t("Add a product…")} value={q} onChange={(e) => setQ(e.target.value)} />
          {rows.map((p) => (
            <div
              key={p.product_id}
              className="result-row"
              onClick={() => {
                if (!lines.some((l) => l.product_id === p.product_id))
                  setLines([...lines, { product_id: p.product_id, name: p.name, qty: "1" }]);
                setQ("");
              }}
            >
              <span className="grow">{p.name}</span>
              <span className="tiny">{t("On hand {0}", formatQty(p.stock_milli))}</span>
            </div>
          ))}
        </div>
        <TextInput label={t("Note")} value={note} onChange={(e) => setNote(e.target.value)} />
        <div className="tiny">
          {t("Nothing moves until the transfer is shipped. Shipped stock is in transit until it is received.")}
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

// ---- Branches --------------------------------------------------------------

export function BranchesPage() {
  return (
    <div>
      <PageHeader
        title={t("Branches")}
        subtitle={t(
          "One hub serves every branch on this network. Each till belongs to one branch, chosen when it is paired.",
        )}
      />
      <FeatureGate feature="org.multi_branch">
        <BranchesBody />
      </FeatureGate>
    </div>
  );
}

function BranchesBody() {
  const { has } = useSession();
  const { data, setData, error } = useLoad(() => api.branches.list(), []);
  const users = useLoad(() => (has("users.manage") ? api.users.list() : Promise.resolve([] as UserRow[])), []);
  const [edit, setEdit] = useState<Branch | "new" | null>(null);
  const [assign, setAssign] = useState<UserRow | null>(null);
  return (
    <div className="col gap-24">
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <div className="row">
        <span className="grow" />
        {has("branches.manage") ? (
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => setEdit("new")}>
            {t("Add branch")}
          </Button>
        ) : null}
      </div>
      {!data ? <Skeleton /> : null}
      {(data ?? []).map((b) => (
        <div key={b.branch_id} className="card card-pad col gap-8" data-testid="branch-card">
          <div className="row">
            <strong>{b.name}</strong>
            <span className="tiny">{b.code}</span>
            {!b.active ? <Chip>{t("Inactive")}</Chip> : null}
            <span className="grow" />
            <span className="small">{t("{0} staff", b.user_count)}</span>
            {has("branches.manage") ? (
              <Button size="sm" onClick={() => setEdit(b)}>
                {t("Edit")}
              </Button>
            ) : null}
          </div>
          <div className="small muted">{[b.address, b.phone].filter(Boolean).join(" · ")}</div>
          <div className="row wrap">
            {b.devices.length === 0 ? (
              <span className="tiny">{t("No devices yet. Pair a till into this branch from Sync / Hub.")}</span>
            ) : null}
            {b.devices.map((d) => (
              <Chip key={d.device_id} tone={d.active ? "info" : "default"}>
                {d.name} · {codeLabel(d.mode)}
              </Chip>
            ))}
          </div>
        </div>
      ))}
      {has("branches.manage") && users.data?.length ? (
        <div className="card card-pad col gap-8">
          <h3>{t("Staff branches")}</h3>
          <div className="tiny">
            {t("Staff work in their home branch plus any branches added here. Owners can work in every branch.")}
          </div>
          <table className="table">
            <tbody>
              {users.data
                .filter((u) => u.active)
                .map((u) => (
                  <tr key={u.user_id}>
                    <td>{u.display_name}</td>
                    <td className="small">{u.role_name}</td>
                    <td className="num">
                      <Button size="sm" onClick={() => setAssign(u)}>
                        {t("Branches")}
                      </Button>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {edit ? (
        <BranchDialog
          branch={edit === "new" ? null : edit}
          onClose={() => setEdit(null)}
          onSaved={(r) => (setData(r), setEdit(null))}
        />
      ) : null}
      {assign && data ? <UserBranchesDialog user={assign} branches={data} onClose={() => setAssign(null)} /> : null}
    </div>
  );
}

function BranchDialog({
  branch,
  onClose,
  onSaved,
}: {
  branch: Branch | null;
  onClose: () => void;
  onSaved: (r: Branch[]) => void;
}) {
  const [code, setCode] = useState(branch?.code ?? "");
  const [name, setName] = useState(branch?.name ?? "");
  const [address, setAddress] = useState(branch?.address ?? "");
  const [phone, setPhone] = useState(branch?.phone ?? "");
  const [active, setActive] = useState(branch?.active ?? true);
  const act = useAction();
  return (
    <Modal
      title={branch ? t("Edit branch") : t("Add branch")}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() =>
                api.branches.save(branch?.branch_id ?? null, {
                  code,
                  name,
                  address: address || null,
                  phone: phone || null,
                  active,
                }),
              );
              if (r) onSaved(r);
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <TextInput label={t("Code")} value={code} onChange={(e) => setCode(e.target.value)} />
        <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} />
        <TextInput label={t("Address")} value={address} onChange={(e) => setAddress(e.target.value)} />
        <TextInput label={t("Phone")} value={phone} onChange={(e) => setPhone(e.target.value)} />
        <Checkbox label={t("Active")} checked={active} onChange={setActive} />
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

function UserBranchesDialog({ user, branches, onClose }: { user: UserRow; branches: Branch[]; onClose: () => void }) {
  const toast = useToast();
  const { data, setData, error } = useLoad(() => api.branches.userGet(user.user_id), [user.user_id]);
  const act = useAction();
  const sel = new Set(data ?? []);
  return (
    <Modal
      title={t("Branches for {0}", user.display_name)}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.branches.userSet(user.user_id, [...sel]));
              if (r) {
                toast("success", t("Saved"));
                onClose();
              }
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-8">
        {error ? <Banner tone="danger">{error}</Banner> : null}
        {branches
          .filter((b) => b.active)
          .map((b) => (
            <Checkbox
              key={b.branch_id}
              label={b.name}
              checked={sel.has(b.branch_id)}
              onChange={(x) => {
                const n = new Set(sel);
                if (x) n.add(b.branch_id);
                else n.delete(b.branch_id);
                setData([...n]);
              }}
            />
          ))}
        <div className="tiny">
          {t("The home branch always stays. Signed-in sessions pick up changes at the next sign-in.")}
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

/** Header control: work in another branch (back office only). */
export function BranchSwitcher() {
  const multi = useFeature("org.multi_branch");
  const { session, setSessionData } = useSession();
  const { data } = useLoad(() => (multi ? api.branches.list() : Promise.resolve([] as Branch[])), [multi]);
  const [error, setError] = useState<string | null>(null);
  if (!multi || !data || data.length < 2) return null;
  return (
    <span className="row" style={{ gap: 4 }} title={error ?? t("Branch")}>
      <select
        className="select"
        style={{ height: 30, width: 160 }}
        aria-label={t("Branch")}
        value={session?.branch_id ?? ""}
        onChange={async (e) => {
          setError(null);
          try {
            setSessionData(await api.branches.switchTo(e.target.value));
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        {data
          .filter((b) => b.active)
          .map((b) => (
            <option key={b.branch_id} value={b.branch_id}>
              {b.name}
            </option>
          ))}
      </select>
    </span>
  );
}

/** Per-branch price overrides on the product editor. */
export function BranchPricesCard({ productId }: { productId: string }) {
  const multi = useFeature("org.multi_branch");
  const { has } = useSession();
  const { data, setData, error } = useLoad(
    () => (multi ? api.branches.prices(productId) : Promise.resolve([] as BranchPrice[])),
    [multi, productId],
  );
  const [edit, setEdit] = useState<Record<string, string>>({});
  const act = useAction();
  if (!multi) return null;
  return (
    <div className="card card-pad col gap-16">
      <h3>{t("Branch prices")}</h3>
      <div className="tiny">
        {t("Leave empty to use the shared price. A branch price applies only to tills in that branch.")}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <table className="table">
        <tbody>
          {(data ?? []).map((b) => (
            <tr key={b.branch_id}>
              <td>{b.name}</td>
              <td className="num">{b.price_minor === null ? t("Shared price") : formatMoney(b.price_minor)}</td>
              {has("prices.manage") ? (
                <td className="num">
                  <div className="row" style={{ justifyContent: "flex-end" }}>
                    <input
                      className="input num"
                      style={{ width: 110 }}
                      aria-label={t("Branch price")}
                      value={edit[b.branch_id] ?? ""}
                      onChange={(e) => setEdit({ ...edit, [b.branch_id]: e.target.value })}
                    />
                    <Button
                      size="sm"
                      disabled={parseMoney(edit[b.branch_id] ?? "") === null}
                      onClick={async () => {
                        const r = await act.run(() =>
                          api.branches.setPrice(productId, b.branch_id, parseMoney(edit[b.branch_id] ?? "")),
                        );
                        if (r) {
                          setData(r);
                          setEdit({ ...edit, [b.branch_id]: "" });
                        }
                      }}
                    >
                      {t("Set")}
                    </Button>
                    {b.price_minor !== null ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={async () => {
                          const r = await act.run(() => api.branches.setPrice(productId, b.branch_id, null));
                          if (r) setData(r);
                        }}
                      >
                        {t("Use shared")}
                      </Button>
                    ) : null}
                  </div>
                </td>
              ) : null}
            </tr>
          ))}
        </tbody>
      </table>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

/** Report branch filter (owners see all branches; others their own). */
export function BranchFilter({ value, onChange }: { value: string; onChange: (b: string) => void }) {
  const multi = useFeature("org.multi_branch");
  const { has } = useSession();
  const { data } = useLoad(() => (multi ? api.branches.list() : Promise.resolve([] as Branch[])), [multi]);
  if (!multi || !data || data.length < 2) return null;
  return (
    <select
      className="select"
      style={{ width: 180 }}
      aria-label={t("Branch")}
      value={value}
      onChange={(e) => onChange(e.target.value)}
    >
      <option value="">{has("branches.all") ? t("All branches") : t("My branch")}</option>
      {data.map((b) => (
        <option key={b.branch_id} value={b.branch_id}>
          {b.name}
        </option>
      ))}
    </select>
  );
}

// ---- Saved date ranges -----------------------------------------------------

export function resolvePreset(p: ReportPreset): [string, string] {
  const today = todayLocal();
  const d = new Date(`${today}T00:00:00`);
  const iso = (x: Date) =>
    `${x.getFullYear()}-${String(x.getMonth() + 1).padStart(2, "0")}-${String(x.getDate()).padStart(2, "0")}`;
  switch (p.range_kind) {
    case "today":
      return [today, today];
    case "yesterday":
      return [todayLocal(-1), todayLocal(-1)];
    case "last_7":
      return [todayLocal(-6), today];
    case "this_week": {
      // Week starts on Saturday (GCC retail).
      const back = (d.getDay() + 1) % 7;
      return [todayLocal(-back), today];
    }
    case "this_month":
      return [today.slice(0, 8) + "01", today];
    case "last_month": {
      const first = new Date(d.getFullYear(), d.getMonth() - 1, 1);
      const last = new Date(d.getFullYear(), d.getMonth(), 0);
      return [iso(first), iso(last)];
    }
    default:
      return [p.from_date ?? today, p.to_date ?? today];
  }
}

/** Saved ranges for this user: apply, save the current range, delete. */
export function SavedRanges({
  from,
  to,
  onApply,
}: {
  from: string;
  to: string;
  onApply: (f: string, t: string) => void;
}) {
  const { data, setData } = useLoad(() => api.reports.presets(), []);
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState("");
  const act = useAction();
  return (
    <div className="row wrap" data-testid="saved-ranges">
      {(data ?? []).map((p) => (
        <span key={p.preset_id} className="filter-chip" style={{ display: "inline-flex", gap: 4 }}>
          <button className="link" onClick={() => onApply(...resolvePreset(p))}>
            {p.name}
          </button>
          <button
            className="link"
            aria-label={t("Delete {0}", p.name)}
            onClick={async () => {
              const r = await act.run(() => api.reports.presetDelete(p.preset_id));
              if (r) setData(r);
            }}
          >
            <X size={12} />
          </button>
        </span>
      ))}
      {naming ? (
        <>
          <input
            className="input"
            style={{ width: 160 }}
            placeholder={t("Name this range")}
            value={name}
            autoFocus
            onChange={(e) => setName(e.target.value)}
          />
          <Button
            size="sm"
            disabled={!name.trim()}
            onClick={async () => {
              const r = await act.run(() =>
                api.reports.presetSave({ name, range_kind: "fixed", from_date: from, to_date: to }),
              );
              if (r) {
                setData(r);
                setNaming(false);
                setName("");
              }
            }}
          >
            {t("Save")}
          </Button>
        </>
      ) : (
        <Button size="sm" variant="ghost" onClick={() => setNaming(true)}>
          {t("Save this range")}
        </Button>
      )}
      {act.error ? <span className="tiny neg-num">{act.error}</span> : null}
    </div>
  );
}

// ---- End of day ------------------------------------------------------------

function MiniReport({ rep }: { rep: Report }) {
  return (
    <div className="card card-pad col gap-8">
      <h3>{tb(rep.title)}</h3>
      <div className="kpis">
        {rep.kpis.map((k) => (
          <div key={k.label} className="card kpi">
            <div className="k-label">{tb(k.label)}</div>
            <div className="k-value">
              {fmtCell({ key: k.label, label: k.label, kind: k.kind as ReportColumn["kind"] }, k.value)}
            </div>
          </div>
        ))}
      </div>
      {rep.rows.length ? (
        <div className="table-wrap" style={{ maxHeight: 320 }}>
          <table className="table">
            <thead>
              <tr>
                {rep.columns.map((c) => (
                  <th key={c.key} className={["money", "qty", "int", "percent_bp"].includes(c.kind) ? "num" : ""}>
                    {tb(c.label)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rep.rows.map((r, i) => (
                <tr key={i}>
                  {rep.columns.map((c) => (
                    <td key={c.key} className={["money", "qty", "int", "percent_bp"].includes(c.kind) ? "num" : ""}>
                      {fmtCell(c, (r as Record<string, unknown>)[c.key])}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="tiny">{t("Nothing recorded.")}</div>
      )}
      {rep.notes.map((n) => (
        <div key={n} className="tiny">
          {tb(n)}
        </div>
      ))}
    </div>
  );
}

export function EndOfDayPage() {
  const nav = useNavigate();
  const aiOn = useFeature("ai.enabled");
  const { has } = useSession();
  const [date, setDate] = useState(todayLocal());
  const [branch, setBranch] = useState("");
  const { data, error } = useLoad<EodPack>(() => api.reports.eod(date, branch || null), [date, branch]);
  const act = useAction();
  const summary = useMemo(() => {
    if (!data?.sales) return "";
    const k = data.sales.kpis;
    return k.map((x) => `${x.label}: ${x.value}`).join("; ");
  }, [data]);
  return (
    <div>
      <PageHeader
        title={t("End of day")}
        subtitle={t(
          "Sales, payments, shift variances, refunds and low stock for one day. Every figure comes from recorded transactions.",
        )}
        actions={
          <Button
            icon={<Download size={16} />}
            loading={act.busy}
            onClick={async () => {
              const z = await act.run(() => api.reports.eodZip(date, branch || null));
              if (z) downloadBase64(z.file_name, z.base64, "application/zip");
            }}
          >
            {t("Download CSV pack (zip)")}
          </Button>
        }
      />
      <div className="row wrap" style={{ marginBottom: 16 }}>
        <input
          type="date"
          className="input"
          style={{ width: 170 }}
          value={date}
          max={todayLocal()}
          aria-label={t("Date")}
          onChange={(e) => setDate(e.target.value)}
        />
        <BranchFilter value={branch} onChange={setBranch} />
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? <Skeleton rows={8} /> : null}
      {data ? (
        <div className="col gap-24">
          {data.sales ? <MiniReport rep={data.sales} /> : null}
          {data.tenders ? <MiniReport rep={data.tenders} /> : null}
          {data.shifts ? <MiniReport rep={data.shifts} /> : null}
          {data.refunds ? <MiniReport rep={data.refunds} /> : null}
          <div className="card card-pad col gap-8">
            <h3>{t("Low stock")}</h3>
            {data.hidden.includes("low_stock") ? <div className="tiny">{t("Not available to your role.")}</div> : null}
            {!data.hidden.includes("low_stock") && data.low_stock.length === 0 ? (
              <div className="tiny">{t("Nothing below its reorder point.")}</div>
            ) : null}
            <table className="table">
              <tbody>
                {data.low_stock.map((r) => (
                  <tr key={r.product_id}>
                    <td>{r.name}</td>
                    <td className="tiny">{r.sku}</td>
                    <td className="num">{formatQty(r.qty_milli)}</td>
                    <td className="num tiny">{t("reorder at {0}", formatQty(r.reorder_point_milli))}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {data.hidden.filter((h) => h !== "low_stock").length ? (
            <div className="tiny">{t("Some sections are hidden because your role cannot see them.")}</div>
          ) : null}
          {aiOn && has("ai.use") ? (
            <div className="card card-pad col gap-8">
              <h3>
                <Bot size={16} style={{ verticalAlign: -2 }} /> {t("AI commentary (optional)")}
              </h3>
              <div className="tiny">
                {t(
                  "Separate from the figures above. The assistant may be wrong; the recorded figures are the reference.",
                )}
              </div>
              <div>
                <Button
                  onClick={() =>
                    nav(
                      `/admin/ai?q=${encodeURIComponent(
                        `Comment on the end-of-day figures for ${data.date}: ${summary}. What stands out?`,
                      )}`,
                    )
                  }
                >
                  {t("Ask the assistant about this day")}
                </Button>
              </div>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

// ---- Owner phone view ------------------------------------------------------

export function PhoneViewPage() {
  return (
    <div>
      <PageHeader
        title={t("Phone view")}
        subtitle={t(
          "A read-only page for the owner's phone on the store network: today's sales, pending deliveries and low stock.",
        )}
      />
      <FeatureGate feature="pwa.companion">
        <PhoneViewBody />
      </FeatureGate>
    </div>
  );
}

function PhoneViewBody() {
  const { data, setData, error } = useLoad(() => api.companion.tokens(), []);
  const addrs = useLoad(() => api.sync.hubAddresses(), []);
  const [hours, setHours] = useState("12");
  const [label, setLabel] = useState("");
  const [link, setLink] = useState<{ url: string; expires_at: string } | null>(null);
  const act = useAction();
  const base = addrs.data?.addresses?.[0] ?? null;
  return (
    <div className="col gap-24">
      <Banner tone="info" title={t("How it works")}>
        {t(
          "The hub serves the page on the store network only. The link carries a secret that expires within 24 hours; anyone with the link can see the figures until then, so send it only to your own phone. Revoke it here at any time.",
        )}
      </Banner>
      {addrs.data && !addrs.data.running ? (
        <Banner tone="warning">
          {t("The hub service is not running on this computer. The phone view needs the hub.")}
        </Banner>
      ) : null}
      <div className="card card-pad col gap-16">
        <h3>{t("New phone link")}</h3>
        <div className="grid-2">
          <TextInput
            label={t("Label")}
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            hint={t("e.g. Owner's phone")}
          />
          <TextInput
            label={t("Valid for (hours)")}
            className="num"
            value={hours}
            onChange={(e) => setHours(e.target.value.replace(/[^\d]/g, ""))}
            hint={t("1 to 24 hours")}
          />
        </div>
        <div>
          <Button
            variant="primary"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.companion.issue(label || null, Number(hours) || 12));
              if (r) {
                const url = `${base ?? ""}${r.path}#t=${r.token}`;
                setLink({ url, expires_at: r.expires_at });
                setData(await api.companion.tokens());
              }
            }}
          >
            {t("Create link")}
          </Button>
        </div>
        {link ? (
          <Banner tone="success" title={t("Open this on the phone (same Wi-Fi as the hub)")}>
            <div className="col gap-8">
              <code className="mono small" style={{ wordBreak: "break-all" }} data-testid="phone-link">
                {link.url}
              </code>
              <div className="tiny">{t("Shown once. Expires {0}.", formatShort(link.expires_at))}</div>
              <div>
                <Button size="sm" onClick={() => void navigator.clipboard?.writeText(link.url)}>
                  {t("Copy link")}
                </Button>
              </div>
            </div>
          </Banner>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
      <div className="card card-pad col gap-8">
        <h3>{t("Active links")}</h3>
        {error ? <Banner tone="danger">{error}</Banner> : null}
        {data && data.length === 0 ? <div className="tiny">{t("No active links.")}</div> : null}
        <table className="table">
          <tbody>
            {(data ?? []).map((tk: CompanionToken) => (
              <tr key={tk.id}>
                <td>{tk.label ?? tk.user_name ?? "—"}</td>
                <td className="small">{t("Expires {0}", formatShort(tk.expires_at))}</td>
                <td className="small">
                  {tk.last_used_at ? t("Last used {0}", formatShort(tk.last_used_at)) : t("Not used yet")}
                </td>
                <td className="num">
                  <Button
                    size="sm"
                    variant="danger-outline"
                    onClick={async () => {
                      const r = await act.run(() => api.companion.revoke(tk.id));
                      if (r) setData(r);
                    }}
                  >
                    {t("Revoke")}
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ---- Loyalty ---------------------------------------------------------------

export function LoyaltySettingsSection() {
  const toast = useToast();
  const { data, setData, error } = useLoad(() => api.settings.get<LoyaltySettings>("loyalty"), []);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  return (
    <FeatureGate feature="loyalty.enabled">
      <div className="card card-pad col gap-16">
        <TextInput
          label={t("Amount paid for one point")}
          className="num"
          value={formatMoney(data.earn_minor_per_point).split(" ")[1] ?? ""}
          hint={t("Customers earn one point for each of this amount paid, after discounts. Points are whole numbers.")}
          onChange={(e) => {
            const v = parseMoney(e.target.value);
            if (v !== null) setData({ ...data, earn_minor_per_point: v });
          }}
        />
        <TextInput
          label={t("Value of one point when redeemed")}
          className="num"
          value={formatMoney(data.redeem_minor_per_point).split(" ")[1] ?? ""}
          hint={t("Redeemed points become a discount on the sale, never cash.")}
          onChange={(e) => {
            const v = parseMoney(e.target.value);
            if (v !== null) setData({ ...data, redeem_minor_per_point: v });
          }}
        />
        <TextInput
          label={t("Minimum points to redeem")}
          className="num"
          value={String(data.min_redeem_points)}
          onChange={(e) => setData({ ...data, min_redeem_points: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
        />
        <Checkbox
          label={t("No points on lines that already have a discount")}
          checked={data.exclude_discounted_lines}
          onChange={(x) => setData({ ...data, exclude_discounted_lines: x })}
        />
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div>
          <Button
            variant="primary"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.settings.save("loyalty", data));
              if (r) toast("success", t("Settings saved"));
            }}
          >
            {t("Save changes")}
          </Button>
        </div>
      </div>
    </FeatureGate>
  );
}

/** Loyalty balance and history on the customer page. */
export function LoyaltyCard({ customerId }: { customerId: string }) {
  const on = useFeature("loyalty.enabled");
  const { has } = useSession();
  const { data, setData, error } = useLoad<LoyaltyCustomer | null>(
    () => (on ? api.loyalty.customer(customerId) : Promise.resolve(null)),
    [on, customerId],
  );
  const [adjusting, setAdjusting] = useState(false);
  const [points, setPoints] = useState("");
  const [note, setNote] = useState("");
  const act = useAction();
  if (!on) return null;
  return (
    <div className="card card-pad col gap-16" data-testid="loyalty-card">
      <div className="row">
        <Star size={16} />
        <h3 className="grow">{t("Loyalty points")}</h3>
        {has("loyalty.adjust") ? (
          <Button size="sm" onClick={() => setAdjusting(true)}>
            {t("Adjust")}
          </Button>
        ) : null}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {data ? (
        <>
          <dl className="kv">
            <dt>{t("Balance")}</dt>
            <dd>{t("{0} points", data.balance)}</dd>
            <dt>{t("Worth")}</dt>
            <dd className="money">{formatMoney(data.value_minor)}</dd>
          </dl>
          <table className="table">
            <tbody>
              {data.entries.map((e) => (
                <tr key={e.entry_id}>
                  <td className="small">{formatShort(e.created_at)}</td>
                  <td>{codeLabel(e.kind)}</td>
                  <td className="small">{e.receipt_number ?? e.note ?? ""}</td>
                  <td className={`num ${e.points < 0 ? "neg-num" : ""}`}>{e.points > 0 ? `+${e.points}` : e.points}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : (
        <Skeleton rows={2} />
      )}
      {adjusting ? (
        <Modal
          title={t("Adjust points")}
          size="sm"
          onClose={() => setAdjusting(false)}
          footer={
            <>
              <Button onClick={() => setAdjusting(false)}>{t("Cancel")}</Button>
              <Button
                variant="primary"
                className="right"
                loading={act.busy}
                disabled={!note.trim() || !Number(points)}
                onClick={async () => {
                  const r = await act.run(() => api.loyalty.adjust(customerId, Number(points), note));
                  if (r) {
                    setData(r);
                    setAdjusting(false);
                    setPoints("");
                    setNote("");
                  }
                }}
              >
                {t("Save")}
              </Button>
            </>
          }
        >
          <div className="col gap-16">
            <TextInput
              label={t("Points (+ to add, − to remove)")}
              className="num"
              value={points}
              onChange={(e) => setPoints(e.target.value.replace(/[^\d-]/g, ""))}
            />
            <TextInput label={t("Reason")} value={note} onChange={(e) => setNote(e.target.value)} />
            <div className="tiny">{t("Recorded in the audit log. A balance can never go below zero.")}</div>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Modal>
      ) : null}
    </div>
  );
}
