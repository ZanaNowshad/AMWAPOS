import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Lock, Plus, ShieldCheck, Unlock } from "lucide-react";
import { api } from "../../api";
import type { PermissionRow, RoleRow, UserRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { formatDateTime, formatShort } from "../../lib/time";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, TextInput } from "../../components/ui";
import { DataTable, Drawer, useAction, useLoad } from "./common";
import { initials } from "../login/Login";
import { t, tb } from "../../i18n";

export function UsersPage() {
  const nav = useNavigate();
  const toast = useToast();
  const { session } = useSession();
  const { data, loading, error, reload } = useLoad(() => api.users.list(), []);
  const roles = useLoad(() => api.roles.list(), []);
  const [edit, setEdit] = useState<UserRow | "new" | null>(null);
  const [name, setName] = useState("");
  const [role, setRole] = useState("");
  const [pin, setPin] = useState("");
  const [active, setActive] = useState(true);
  const act = useAction();
  const open = (u: UserRow | "new") => {
    setEdit(u);
    setName(u === "new" ? "" : u.display_name);
    setRole(u === "new" ? "role_cashier" : u.role_id);
    setPin("");
    setActive(u === "new" ? true : u.active);
    act.setError(null);
  };
  const save = async () => {
    if (edit === "new") {
      const r = await act.run(() => api.users.create({ display_name: name, role_id: role, pin, active }));
      if (r) toast("success", t("User {0} created", r.display_name));
      else return;
    } else if (edit) {
      const r = await act.run(() =>
        api.users.update(edit.user_id, { display_name: name, role_id: role, pin: pin || null, active }),
      );
      if (r) toast("success", t("User saved"));
      else return;
    }
    setEdit(null);
    void reload();
  };
  const now = new Date().toISOString();
  return (
    <div>
      <PageHeader
        title={t("Users")}
        actions={
          <>
            <Button icon={<ShieldCheck size={16} />} onClick={() => nav("/admin/roles")}>
              {t("Roles & Permissions")}
            </Button>
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => open("new")}>
              {t("User")}
            </Button>
          </>
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<UserRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.user_id}
        onRowClick={open}
        columns={[
          {
            key: "n",
            label: t("Name"),
            render: (r) => (
              <span className="row">
                <span className="avatar sm">{initials(r.display_name)}</span>
                {r.display_name} {r.user_id === session?.user_id ? <Chip tone="brand">{t("You")}</Chip> : null}
              </span>
            ),
            sort: (r) => r.display_name,
          },
          { key: "r", label: t("Role"), render: (r) => tb(r.role_name), sort: (r) => r.role_name },
          { key: "l", label: t("Last Login"), render: (r) => formatShort(r.last_login_at) },
          {
            key: "s",
            label: t("Status"),
            render: (r) =>
              !r.active ? (
                <Chip>{t("Inactive")}</Chip>
              ) : r.locked_until && r.locked_until > now ? (
                <Chip tone="danger">
                  <Lock size={12} /> {t("Locked")}
                </Chip>
              ) : (
                <Chip tone="success">{t("Active")}</Chip>
              ),
          },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              r.locked_until && r.locked_until > now ? (
                <Button
                  size="sm"
                  icon={<Unlock size={14} />}
                  onClick={async (e) => {
                    e.stopPropagation();
                    await act.run(() => api.users.unlock(r.user_id));
                    void reload();
                  }}
                >
                  {t("Unlock")}
                </Button>
              ) : null,
          },
        ]}
      />
      {edit ? (
        <Drawer title={edit === "new" ? t("New user") : t("Edit {0}", edit.display_name)} onClose={() => setEdit(null)}>
          <div className="col gap-16">
            <TextInput label={t("Name")} required value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <Field label={t("Role")} required>
              <select className="select" value={role} onChange={(e) => setRole(e.target.value)}>
                {(roles.data ?? []).map((r) => (
                  <option key={r.role_id} value={r.role_id}>
                    {r.name}
                  </option>
                ))}
              </select>
            </Field>
            <TextInput
              label={edit === "new" ? t("PIN") : t("Reset PIN")}
              required={edit === "new"}
              type="password"
              inputMode="numeric"
              autoComplete="new-password"
              value={pin}
              onChange={(e) => setPin(e.target.value.replace(/\D/g, "").slice(0, 12))}
              hint={
                edit === "new"
                  ? t("4–8 digits. The user should change it after first login.")
                  : t("Leave empty to keep the current PIN. PINs are never shown.")
              }
            />
            <Checkbox label={t("Active")} checked={active} onChange={setActive} />
            {edit !== "new" ? (
              <div className="tiny">
                {t("Created {0} · failed PIN attempts {1}", formatDateTime(edit.created_at), edit.failed_attempts)}
              </div>
            ) : null}
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            <Button
              variant="primary"
              onClick={save}
              loading={act.busy}
              disabled={!name.trim() || (edit === "new" && pin.length < 4)}
            >
              {t("Save")}
            </Button>
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

export function RolesPage() {
  const toast = useToast();
  const { has } = useSession();
  const roles = useLoad(() => api.roles.list(), []);
  const perms = useLoad(() => api.roles.permissions(), []);
  const [sel, setSel] = useState<RoleRow | "new" | null>(null);
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const act = useAction();
  useEffect(() => {
    if (!sel && roles.data?.length) setSel(roles.data[0]);
  }, [roles.data, sel]);
  useEffect(() => {
    if (!sel) return;
    setName(sel === "new" ? "" : sel.name);
    setDesc(sel === "new" ? "" : (sel.description ?? ""));
    setChecked(new Set(sel === "new" ? [] : sel.permissions));
  }, [sel]);
  const domains = useMemo(() => {
    const m = new Map<string, PermissionRow[]>();
    for (const p of perms.data ?? []) m.set(p.domain, [...(m.get(p.domain) ?? []), p]);
    return Array.from(m.entries());
  }, [perms.data]);
  if (!roles.data || !perms.data) return <Skeleton />;
  const owner = sel !== "new" && sel?.role_id === "role_owner";
  const canEdit = has("roles.manage") && !owner;
  return (
    <div>
      <PageHeader
        title={t("Roles & Permissions")}
        subtitle={t(
          "Permissions are enforced by the backend for every action. Hiding a button is never the only protection.",
        )}
        actions={
          has("roles.manage") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setSel("new")}>
              {t("Role")}
            </Button>
          ) : null
        }
      />
      <div className="settings-layout">
        <div className="subnav">
          {roles.data.map((r) => (
            <button
              key={r.role_id}
              className={sel !== "new" && sel?.role_id === r.role_id ? "active" : ""}
              onClick={() => setSel(r)}
            >
              {r.name} <span className="tiny">({r.user_count})</span>
            </button>
          ))}
        </div>
        <div className="card card-pad col gap-16">
          <div className="form-grid">
            <TextInput
              label={t("Role name")}
              value={name}
              onChange={(e) => setName(e.target.value)}
              disabled={!canEdit}
            />
            <TextInput
              label={t("Description")}
              value={desc}
              onChange={(e) => setDesc(e.target.value)}
              disabled={!canEdit}
            />
          </div>
          {owner ? (
            <Banner tone="info">{t("The Owner role always has every permission and cannot be edited.")}</Banner>
          ) : null}
          <div className="perm-matrix">
            {domains.map(([domain, ps]) => (
              <div key={domain}>
                <h3 style={{ marginBottom: 6 }}>{domain}</h3>
                <div className="col" style={{ gap: 4 }}>
                  {ps.map((p) => (
                    <Checkbox
                      key={p.code}
                      label={
                        <span>
                          {p.description} <span className="tiny mono">{p.code}</span>
                        </span>
                      }
                      checked={checked.has(p.code)}
                      disabled={!canEdit}
                      onChange={(v) => {
                        const s = new Set(checked);
                        if (v) s.add(p.code);
                        else s.delete(p.code);
                        setChecked(s);
                      }}
                    />
                  ))}
                </div>
              </div>
            ))}
          </div>
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {canEdit ? (
            <div className="row">
              <span className="tiny">
                {t("Changing a role signs out its users so the new permissions apply immediately.")}
              </span>
              <Button
                variant="primary"
                className="right"
                loading={act.busy}
                disabled={!name.trim()}
                onClick={async () => {
                  const r = await act.run(() =>
                    api.roles.save(sel === "new" ? null : sel!.role_id, name, desc || null, Array.from(checked)),
                  );
                  if (r) {
                    toast("success", t("Role saved"));
                    await roles.reload();
                    setSel(r.find((x) => x.name === name) ?? null);
                  }
                }}
              >
                {t("Save role")}
              </Button>
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

export function ProfilePage() {
  const { session } = useSession();
  const toast = useToast();
  const [cur, setCur] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const act = useAction();
  if (!session) return null;
  return (
    <div className="grid-2">
      <div className="card card-pad col gap-16">
        <div className="row">
          <span className="avatar">{initials(session.display_name)}</span>
          <div>
            <h2>{session.display_name}</h2>
            <div className="muted">{tb(session.role_name)}</div>
          </div>
        </div>
        <dl className="kv">
          <dt>{t("Signed in")}</dt>
          <dd>{formatDateTime(session.created_at)}</dd>
          <dt>{t("Permissions")}</dt>
          <dd>{session.permissions.length}</dd>
        </dl>
      </div>
      <div className="card card-pad col gap-16">
        <h3>{t("Change PIN")}</h3>
        <TextInput
          label={t("Current PIN")}
          type="password"
          inputMode="numeric"
          value={cur}
          onChange={(e) => setCur(e.target.value.replace(/\D/g, ""))}
        />
        <TextInput
          label={t("New PIN")}
          type="password"
          inputMode="numeric"
          value={next}
          onChange={(e) => setNext(e.target.value.replace(/\D/g, ""))}
        />
        <TextInput
          label={t("Confirm new PIN")}
          type="password"
          inputMode="numeric"
          value={again}
          onChange={(e) => setAgain(e.target.value.replace(/\D/g, ""))}
          error={again && again !== next ? t("PINs do not match.") : null}
        />
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <Button
          variant="primary"
          disabled={!cur || next.length < 4 || next !== again}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() => api.auth.changePin(cur, next));
            if (r !== undefined) {
              toast("success", t("PIN changed"));
              setCur("");
              setNext("");
              setAgain("");
            }
          }}
        >
          {t("Change PIN")}
        </Button>
      </div>
    </div>
  );
}
