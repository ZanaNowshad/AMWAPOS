// Business Memory (Wave 8): what is true about this business but lives in no
// record. Each fact is one sentence with where it came from and who confirmed
// it. The assistant only suggests; a person confirms. Records always win: a
// memory whose record changed, ended or was not checked for a long time says
// "May be outdated". Memory never authorises anything.
import { useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { Brain, Plus } from "lucide-react";
import { api } from "../../api";
import type { MemoryDetail, MemoryInput, MemoryRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Chip, Empty, Field, Modal, PageHeader, Skeleton, Tabs, TextInput } from "../../components/ui";
import { Drawer, useAction, useLoad } from "./common";
import { ENTITY_LABEL, entityLink } from "./library";
import { formatDate, formatDateTime } from "../../lib/time";
import { t } from "../../i18n";

type Tab = "confirmed" | "review" | "archived" | "history";

const SOURCE_LABEL: Record<MemoryRow["source_kind"], () => string> = {
  person: () => t("Written by a person"),
  assistant: () => t("Suggested by the assistant"),
  document: () => t("From a document"),
};

function statusChip(s: MemoryRow["status"]) {
  switch (s) {
    case "confirmed":
      return <Chip tone="success">{t("Confirmed")}</Chip>;
    case "candidate":
      return <Chip tone="warning">{t("Needs review")}</Chip>;
    case "archived":
      return <Chip>{t("Archived")}</Chip>;
    case "superseded":
      return <Chip>{t("Replaced")}</Chip>;
    default:
      return <Chip tone="danger">{t("Rejected")}</Chip>;
  }
}

/** Shown wherever a memory is: records win over memory. */
export function OutdatedNote({ text }: { text: string | null | undefined }) {
  if (!text) return null;
  return (
    <Banner tone="warning">
      <span data-testid="memory-outdated">{outdatedLabel(text)}</span>
    </Banner>
  );
}

/** The backend says why in English; show the reason in the person's language. */
function outdatedLabel(text: string): string {
  if (text.includes("no longer exists")) return t("May be outdated: the record it is about no longer exists.");
  if (text.includes("switched off")) return t("May be outdated: the record it is about is switched off.");
  if (text.includes("changed after"))
    return t("May be outdated: the record it is about changed after this was confirmed. The record is right.");
  if (text.includes("valid until"))
    return t("May be outdated: it was valid until {0}.", text.replace(/.*valid until /, "").replace(/\.$/, ""));
  if (text.includes("nobody has checked")) return t("May be outdated: nobody has checked it for more than 180 days.");
  return t("May be outdated");
}

export function MemoryPage() {
  const { has } = useSession();
  const [params, setParams] = useSearchParams();
  const [tab, setTab] = useState<Tab>("confirmed");
  const [q, setQ] = useState("");
  const [query, setQuery] = useState("");
  const [adding, setAdding] = useState(false);
  const list = useLoad(() => api.memory.list({ tab, q: query || null, limit: 200 }), [tab, query]);
  const open = params.get("m");
  const setOpen = (id: string | null) => {
    const p = new URLSearchParams(params);
    if (id) p.set("m", id);
    else p.delete("m");
    setParams(p, { replace: true });
  };
  const counts = list.data?.counts;
  return (
    <div>
      <PageHeader
        title={t("Business Memory")}
        subtitle={t(
          "Facts about this business that no record holds, each with where it came from and who confirmed it. Records always win; memory never authorises anything.",
        )}
        actions={
          has("memory.manage") ? (
            <Button
              variant="primary"
              icon={<Plus size={18} />}
              onClick={() => setAdding(true)}
              data-testid="memory-add"
            >
              {t("Write down a fact")}
            </Button>
          ) : null
        }
      />
      <form
        className="row wrap gap-8"
        onSubmit={(e) => {
          e.preventDefault();
          setQuery(q);
        }}
      >
        <TextInput
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            if (!e.target.value.trim()) setQuery("");
          }}
          placeholder={t("Search what the business knows")}
          aria-label={t("Search")}
          style={{ minWidth: 320 }}
        />
        <Button type="submit">{t("Search")}</Button>
      </form>
      <div style={{ marginTop: 12 }}>
        <Tabs<Tab>
          value={tab}
          onChange={setTab}
          tabs={[
            { key: "confirmed", label: `${t("Confirmed")} (${counts?.confirmed ?? 0})` },
            { key: "review", label: `${t("Needs review")} (${counts?.review ?? 0})` },
            { key: "archived", label: `${t("Archived")} (${counts?.archived ?? 0})` },
            { key: "history", label: t("History") },
          ]}
        />
      </div>
      <div style={{ marginTop: 16 }} data-testid="memory-list">
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        {list.loading && !list.data ? (
          <Skeleton rows={5} />
        ) : list.data?.rows.length === 0 ? (
          tab === "review" ? (
            <Empty title={t("Nothing to review")}>
              {t(
                "Facts the assistant suggests, or that someone takes from a document, wait here until a person confirms them.",
              )}
            </Empty>
          ) : tab === "confirmed" && !query ? (
            <Empty title={t("Nothing written down yet")}>
              {t(
                "Write down what the team knows but no record holds: delivery days, a landlord's preferences, what sells before Eid. Business Memory starts empty; nothing is guessed.",
              )}
            </Empty>
          ) : (
            <Empty title={t("Nothing here")} />
          )
        ) : (
          <div className="col gap-8">
            {list.data?.rows.map((m) => (
              <button
                key={m.memory_id}
                className="card card-pad col gap-4 text-start"
                onClick={() => setOpen(m.memory_id)}
                data-testid="memory-row"
              >
                <div className="row wrap gap-8">
                  <span dir="auto" className="grow">
                    {m.statement}
                  </span>
                  {statusChip(m.status)}
                </div>
                <div className="row wrap gap-8 tiny muted">
                  <span dir="ltr">{m.number}</span>
                  {m.entity_type ? (
                    <span>
                      {ENTITY_LABEL[m.entity_type]?.()}: <span dir="auto">{m.entity_label ?? "—"}</span>
                    </span>
                  ) : null}
                  <span>{SOURCE_LABEL[m.source_kind]?.()}</span>
                  {m.from_untrusted ? <Chip tone="warning">{t("After reading outside text")}</Chip> : null}
                  {m.outdated ? <Chip tone="warning">{t("May be outdated")}</Chip> : null}
                </div>
              </button>
            ))}
          </div>
        )}
      </div>
      {adding ? (
        <MemoryForm
          onClose={() => setAdding(false)}
          onSaved={(m) => {
            setAdding(false);
            void list.reload();
            setOpen(m.memory_id);
          }}
        />
      ) : null}
      {open ? <MemoryDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
    </div>
  );
}

/** Write down a fact (confirmed by the person), suggest one from a
 *  document, or change one (a confirmed fact is replaced, never rewritten). */
export function MemoryForm({
  initial,
  edit,
  fromDocument,
  onClose,
  onSaved,
}: {
  initial?: MemoryInput;
  edit?: { memory_id: string; revision: number; confirmed: boolean };
  fromDocument?: { document_id: string; excerpt: string };
  onClose: () => void;
  onSaved: (m: MemoryRow) => void;
}) {
  const toast = useToast();
  const act = useAction();
  const [statement, setStatement] = useState(initial?.statement ?? "");
  const [validUntil, setValidUntil] = useState(initial?.valid_until ?? "");
  const [branch, setBranch] = useState(initial?.scope === "branch");
  const [excerpt, setExcerpt] = useState(fromDocument?.excerpt ?? "");
  const save = async () => {
    const input: MemoryInput = {
      statement,
      entity_type: initial?.entity_type ?? null,
      entity_id: initial?.entity_id ?? null,
      scope: branch ? "branch" : "business",
      valid_until: validUntil || null,
      document_id: fromDocument?.document_id ?? null,
      excerpt: fromDocument ? excerpt : null,
    };
    const r = await act.run(() =>
      edit ? api.memory.edit(edit.memory_id, edit.revision, input) : api.memory.add(input, !fromDocument),
    );
    if (r) {
      toast("success", fromDocument ? t("Saved as a suggestion to review") : t("Saved"));
      onSaved(r);
    }
  };
  return (
    <Modal
      title={
        edit ? t("Change this fact") : fromDocument ? t("Suggest a fact from this document") : t("Write down a fact")
      }
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            disabled={statement.trim().length < 3}
            loading={act.busy}
            onClick={save}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-12" data-testid="memory-form">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {edit?.confirmed ? (
          <span className="tiny muted">{t("The confirmed fact stays in the history; this one replaces it.")}</span>
        ) : null}
        <Field label={t("The fact, in one sentence")} hint={t("No passwords, codes or personal phone numbers.")}>
          <textarea
            className="input"
            rows={3}
            maxLength={500}
            dir="auto"
            value={statement}
            onChange={(e) => setStatement(e.target.value)}
            aria-label={t("The fact, in one sentence")}
          />
        </Field>
        {fromDocument ? (
          <Field label={t("The passage it comes from")}>
            <textarea
              className="input"
              rows={3}
              maxLength={500}
              dir="auto"
              value={excerpt}
              onChange={(e) => setExcerpt(e.target.value)}
            />
          </Field>
        ) : null}
        <label className="row gap-8">
          <input type="checkbox" checked={branch} onChange={(e) => setBranch(e.target.checked)} />
          {t("Only for this branch")}
        </label>
        <Field label={t("True until")} hint={t("Optional. After this date it is shown as possibly outdated.")}>
          <input type="date" className="input" value={validUntil} onChange={(e) => setValidUntil(e.target.value)} />
        </Field>
      </div>
    </Modal>
  );
}

function MemoryCard({ m, onOpen }: { m: MemoryRow; onOpen: (id: string) => void }) {
  return (
    <button className="card card-pad col gap-4 text-start" onClick={() => onOpen(m.memory_id)}>
      <span dir="auto">{m.statement}</span>
      <span className="tiny muted">
        <span dir="ltr">{m.number}</span> · {m.confirmed_at ? formatDate(m.confirmed_at) : ""}
      </span>
    </button>
  );
}

export function MemoryDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const nav = useNavigate();
  const toast = useToast();
  const [current, setCurrent] = useState(id);
  const { data, error, reload } = useLoad<MemoryDetail>(() => api.memory.get(current), [current]);
  const act = useAction();
  const [note, setNote] = useState("");
  const [editing, setEditing] = useState(false);
  const manage = has("memory.manage");
  const m = data?.memory;
  const decide = async (action: "confirm" | "reject" | "archive" | "restore" | "verify") => {
    if (!m) return;
    const r = await act.run(() => api.memory.decide(m.memory_id, action, m.revision, note.trim() || null));
    if (r) {
      setNote("");
      toast("success", t("Saved"));
      onChanged();
      void reload();
    }
  };
  return (
    <Drawer wide title={m ? m.number : t("Business Memory")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data || !m ? (
        <Skeleton rows={5} />
      ) : (
        <div className="col gap-16" data-testid="memory-drawer">
          <div className="card card-pad col gap-8">
            <p dir="auto" style={{ fontSize: 18, margin: 0 }}>
              {m.statement}
            </p>
            <div className="row wrap gap-8">
              {statusChip(m.status)}
              <Chip>{m.scope === "branch" ? t("This branch only") : t("The whole business")}</Chip>
              {m.valid_until ? <Chip>{t("True until {0}", formatDate(m.valid_until))}</Chip> : null}
            </div>
          </div>
          <OutdatedNote text={m.outdated} />
          {m.status === "candidate" && data.related.length ? (
            <div className="card card-pad col gap-8" data-testid="memory-related">
              <Banner tone="warning">
                {t("Check these confirmed facts before confirming: they are about the same thing and may disagree.")}
              </Banner>
              {data.related.map((r) => (
                <MemoryCard key={r.memory_id} m={r} onOpen={setCurrent} />
              ))}
            </div>
          ) : null}
          <table className="table">
            <tbody>
              {m.entity_type ? (
                <tr>
                  <td className="tiny">{t("About")}</td>
                  <td>
                    <Button size="sm" variant="ghost" onClick={() => nav(entityLink(m.entity_type!, m.entity_id!))}>
                      {ENTITY_LABEL[m.entity_type]?.()}: <span dir="auto">{m.entity_label ?? m.entity_id}</span>
                    </Button>
                  </td>
                </tr>
              ) : null}
              <tr>
                <td className="tiny">{t("Where it came from")}</td>
                <td>
                  {SOURCE_LABEL[m.source_kind]?.()}
                  {m.source_kind === "document" && m.source_ref ? (
                    <Button size="sm" variant="ghost" onClick={() => nav(`/admin/documents?doc=${m.source_ref}`)}>
                      {t("Open the document")}
                    </Button>
                  ) : null}
                  {m.from_untrusted ? (
                    <div className="tiny">
                      <Chip tone="warning">{t("After reading outside text")}</Chip>{" "}
                      {t(
                        "The assistant had read text from outside the store in that conversation. Check it carefully.",
                      )}
                    </div>
                  ) : null}
                </td>
              </tr>
              {m.source_excerpt ? (
                <tr>
                  <td className="tiny">{t("The words it came from")}</td>
                  <td dir="auto" className="tiny" style={{ whiteSpace: "pre-wrap" }}>
                    {m.source_excerpt}
                  </td>
                </tr>
              ) : null}
              <tr>
                <td className="tiny">{t("Suggested")}</td>
                <td>
                  {formatDateTime(m.proposed_at)} ·{" "}
                  {m.proposed_by === "assistant" ? t("The assistant") : (m.proposed_by_name ?? "")}
                </td>
              </tr>
              {m.confirmed_at ? (
                <tr>
                  <td className="tiny">{t("Confirmed")}</td>
                  <td>
                    {formatDateTime(m.confirmed_at)} · {m.confirmed_by_name ?? ""}
                  </td>
                </tr>
              ) : null}
              {m.last_verified_at ? (
                <tr>
                  <td className="tiny">{t("Last checked")}</td>
                  <td>{formatDateTime(m.last_verified_at)}</td>
                </tr>
              ) : null}
              {m.decision_note ? (
                <tr>
                  <td className="tiny">{t("Note")}</td>
                  <td dir="auto">{m.decision_note}</td>
                </tr>
              ) : null}
            </tbody>
          </table>
          {m.superseded_by ? (
            <Banner tone="info">
              {t("A newer fact replaced this one.")}{" "}
              <Button size="sm" variant="ghost" onClick={() => setCurrent(m.superseded_by!)}>
                {t("Open the current fact")}
              </Button>
            </Banner>
          ) : null}
          {data.earlier.length ? (
            <div className="card card-pad col gap-8">
              <h3>{t("Earlier versions")}</h3>
              {data.earlier.map((r) => (
                <MemoryCard key={r.memory_id} m={r} onOpen={setCurrent} />
              ))}
            </div>
          ) : null}
          {manage && (m.status === "candidate" || m.status === "confirmed" || m.status === "archived") ? (
            <div className="card card-pad col gap-8">
              <TextInput
                label={m.status === "candidate" ? t("Note (needed to reject)") : t("Note")}
                value={note}
                onChange={(e) => setNote(e.target.value)}
                maxLength={300}
              />
              <div className="row wrap gap-8">
                {m.status === "candidate" ? (
                  <>
                    <Button variant="primary" onClick={() => void decide("confirm")} data-testid="memory-confirm">
                      {t("Confirm: this is true")}
                    </Button>
                    <Button onClick={() => setEditing(true)}>{t("Correct the wording")}</Button>
                    <Button variant="danger" onClick={() => void decide("reject")}>
                      {t("Reject")}
                    </Button>
                  </>
                ) : null}
                {m.status === "confirmed" ? (
                  <>
                    <Button variant="primary" onClick={() => void decide("verify")} data-testid="memory-verify">
                      {t("Still true")}
                    </Button>
                    <Button onClick={() => setEditing(true)}>{t("Change")}</Button>
                    <Button onClick={() => void decide("archive")}>{t("Archive")}</Button>
                  </>
                ) : null}
                {m.status === "archived" ? (
                  <Button onClick={() => void decide("restore")}>{t("Restore")}</Button>
                ) : null}
              </div>
            </div>
          ) : null}
        </div>
      )}
      {editing && m ? (
        <MemoryForm
          initial={{
            statement: m.statement,
            entity_type: m.entity_type,
            entity_id: m.entity_id,
            scope: m.scope,
            valid_until: m.valid_until,
          }}
          edit={{ memory_id: m.memory_id, revision: m.revision, confirmed: m.status === "confirmed" }}
          onClose={() => setEditing(false)}
          onSaved={(r) => {
            setEditing(false);
            onChanged();
            setCurrent(r.memory_id);
            void reload();
          }}
        />
      ) : null}
    </Drawer>
  );
}

/** Confirmed facts about one record, on that record's screen. */
export function LinkedMemories({ entityType, entityId }: { entityType: MemoryInput["entity_type"]; entityId: string }) {
  const { has } = useSession();
  const [open, setOpen] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const list = useLoad(
    () =>
      has("memory.view")
        ? api.memory
            .list({ tab: "confirmed", entity_type: entityType, entity_id: entityId, limit: 50 })
            .then((l) => l.rows)
        : Promise.resolve([] as MemoryRow[]),
    [entityType, entityId],
  );
  if (!has("memory.view")) return null;
  return (
    <div className="card card-pad col gap-8" data-testid="linked-memories">
      <div className="row">
        <h3 className="grow">
          <Brain size={16} /> {t("Business Memory")}
        </h3>
        {has("memory.manage") ? (
          <Button size="sm" icon={<Plus size={16} />} onClick={() => setAdding(true)}>
            {t("Write down a fact")}
          </Button>
        ) : null}
      </div>
      {list.data && list.data.length === 0 ? (
        <span className="muted">{t("Nothing written down about this record.")}</span>
      ) : null}
      {list.data?.map((m) => (
        <button key={m.memory_id} className="col gap-4 text-start link-row" onClick={() => setOpen(m.memory_id)}>
          <span dir="auto">{m.statement}</span>
          {m.outdated ? <span className="tiny warning">{t("May be outdated")}</span> : null}
        </button>
      ))}
      {adding ? (
        <MemoryForm
          initial={{ statement: "", entity_type: entityType, entity_id: entityId }}
          onClose={() => setAdding(false)}
          onSaved={() => {
            setAdding(false);
            void list.reload();
          }}
        />
      ) : null}
      {open ? <MemoryDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
    </div>
  );
}
