// Registers and cash drawers. A register is the checkout ("Till 1"); it
// points at the computer that stands for it now, so a replaced computer can
// take over. Each register has a drawer; shifts record both.
import { useState } from "react";
import { Plus } from "lucide-react";
import { api } from "../../api";
import type { Register } from "../../api/types";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Field, Modal, PageHeader, Skeleton, TextInput } from "../../components/ui";
import { DataTable, useAction, useLoad } from "./common";
import { formatShort } from "../../lib/time";
import { t } from "../../i18n";

export function RegistersPage() {
  const list = useLoad(() => api.registers.list(), []);
  const [edit, setEdit] = useState<Register | "new" | null>(null);
  const [drawerFor, setDrawerFor] = useState<Register | null>(null);
  return (
    <div>
      <PageHeader
        title={t("Registers")}
        subtitle={t(
          "Each checkout and its cash drawer. A register belongs to the computer that stands for it now; a new computer can take over an existing register.",
        )}
        actions={
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => setEdit("new")}>
            {t("Add register")}
          </Button>
        }
      />
      {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
      {!list.data ? (
        <Skeleton />
      ) : (
        <DataTable<Register>
          rows={list.data}
          loading={list.loading}
          rowKey={(r) => r.register_id}
          onRowClick={(r) => setEdit(r)}
          columns={[
            {
              key: "n",
              label: t("Register"),
              render: (r) => (
                <span>
                  <strong>{r.name}</strong>{" "}
                  <span className="tiny" dir="ltr">
                    {r.code}
                  </span>
                </span>
              ),
            },
            {
              key: "c",
              label: t("Computer"),
              render: (r) => r.device_name ?? <span className="tiny">{t("None")}</span>,
            },
            {
              key: "d",
              label: t("Drawers"),
              render: (r) => (
                <span className="row wrap gap-4">
                  {r.drawers.map((d) => (
                    <Chip key={d.drawer_id} tone={d.is_default ? "brand" : "default"}>
                      {d.name}
                      {d.active ? "" : ` (${t("off")})`}
                    </Chip>
                  ))}
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={(e) => {
                      e.stopPropagation();
                      setDrawerFor(r);
                    }}
                  >
                    {t("Add drawer")}
                  </Button>
                </span>
              ),
            },
            {
              key: "s",
              label: t("Now"),
              render: (r) =>
                r.open_shift ? (
                  <span>
                    <Chip tone="success" dot>
                      {r.open_shift.cashier_name}
                    </Chip>{" "}
                    <span className="tiny">{t("since {0}", formatShort(r.open_shift.opened_at))}</span>
                  </span>
                ) : r.active ? (
                  <span className="tiny">{t("Closed")}</span>
                ) : (
                  <Chip>{t("Switched off")}</Chip>
                ),
            },
          ]}
        />
      )}
      {edit ? (
        <RegisterEditor
          reg={edit === "new" ? null : edit}
          onClose={() => setEdit(null)}
          onSaved={() => {
            setEdit(null);
            void list.reload();
          }}
        />
      ) : null}
      {drawerFor ? (
        <DrawerEditor
          reg={drawerFor}
          onClose={() => setDrawerFor(null)}
          onSaved={() => {
            setDrawerFor(null);
            void list.reload();
          }}
        />
      ) : null}
    </div>
  );
}

function RegisterEditor({ reg, onClose, onSaved }: { reg: Register | null; onClose: () => void; onSaved: () => void }) {
  const devices = useLoad(() => api.registers.devices(), []);
  const [name, setName] = useState(reg?.name ?? "");
  const [code, setCode] = useState(reg?.code ?? "");
  const [device, setDevice] = useState(reg?.device_id ?? "");
  const [active, setActive] = useState(reg?.active ?? true);
  const act = useAction();
  const toast = useToast();
  return (
    <Modal
      title={reg ? t("Register {0}", reg.name) : t("Add register")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            loading={act.busy}
            disabled={!name.trim()}
            onClick={async () => {
              const r = await act.run(() =>
                api.registers.save(reg?.register_id ?? null, {
                  name,
                  code: code || null,
                  device_id: device || null,
                  active,
                }),
              );
              if (r) {
                toast("success", t("Saved"));
                onSaved();
              }
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} />
        <TextInput
          label={t("Code")}
          value={code}
          onChange={(e) => setCode(e.target.value)}
          hint={t("Short code printed on reports, for example T1.")}
        />
        <Field
          label={t("Computer")}
          hint={t(
            "Shifts opened on this computer use this register. Choosing a computer moves it from its old register.",
          )}
        >
          <select className="select" value={device} onChange={(e) => setDevice(e.target.value)}>
            <option value="">{t("None")}</option>
            {(devices.data ?? []).map((d) => (
              <option key={d.device_id} value={d.device_id}>
                {d.name} ({d.device_code})
              </option>
            ))}
          </select>
        </Field>
        {reg ? <Checkbox label={t("In use")} checked={active} onChange={setActive} /> : null}
      </div>
    </Modal>
  );
}

function DrawerEditor({ reg, onClose, onSaved }: { reg: Register; onClose: () => void; onSaved: () => void }) {
  const [name, setName] = useState("");
  const [makeDefault, setMakeDefault] = useState(true);
  const act = useAction();
  return (
    <Modal
      title={t("Add a drawer to {0}", reg.name)}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            loading={act.busy}
            disabled={!name.trim()}
            onClick={async () => {
              const r = await act.run(() =>
                api.registers.saveDrawer(null, { register_id: reg.register_id, name, make_default: makeDefault }),
              );
              if (r) onSaved();
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} />
        <Checkbox label={t("Use this drawer for new shifts")} checked={makeDefault} onChange={setMakeDefault} />
      </div>
    </Modal>
  );
}
