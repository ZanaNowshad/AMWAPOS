// The Document Library (Wave 8): evidence files kept with the records they
// support. A file is kept once (its SHA-256 is its fingerprint); a document
// links to the records it is evidence for and never copies their amounts.
// A document's file never changes: replacing it adds a new version and the
// old one stays. Search works on this computer, with no assistant.
import { useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { FileText, Upload } from "lucide-react";
import { api } from "../../api";
import type {
  LibraryAddInput,
  LibraryCategory,
  LibraryDocument,
  LibraryEntityType,
  LibraryHit,
  LibraryRow,
  LibraryTextStatus,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Chip, Empty, Field, Modal, PageHeader, Skeleton, Tabs, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, downloadBase64, useAction, useLoad } from "./common";
import { fileToBase64 } from "./automation";
import { formatDate, formatDateTime } from "../../lib/time";
import { t, tb } from "../../i18n";

export const CATEGORY_LABEL: Record<LibraryCategory, () => string> = {
  invoice: () => t("Invoice"),
  credit_note: () => t("Credit note"),
  receipt: () => t("Receipt"),
  delivery_note: () => t("Delivery note"),
  quotation: () => t("Quotation"),
  price_list: () => t("Price list"),
  statement: () => t("Statement"),
  contract: () => t("Contract"),
  licence: () => t("Licence or permit"),
  insurance: () => t("Insurance"),
  bank: () => t("Bank paper"),
  tax: () => t("Tax paper"),
  other: () => t("Other"),
};
const CATEGORIES = Object.keys(CATEGORY_LABEL) as LibraryCategory[];

export const ENTITY_LABEL: Record<LibraryEntityType, () => string> = {
  supplier: () => t("Supplier"),
  supplier_invoice: () => t("Supplier invoice"),
  purchase_order: () => t("Purchase order"),
  expense: () => t("Expense"),
  product: () => t("Product"),
  case: () => t("Case"),
  day_close: () => t("Z close"),
  supplier_return: () => t("Supplier return"),
  requisition: () => t("Requisition"),
  customer: () => t("Customer"),
  promotion: () => t("Offer"),
};

/** The screen that shows a linked record. */
export function entityLink(type: LibraryEntityType, id: string): string {
  switch (type) {
    case "supplier":
      return `/admin/suppliers/${id}`;
    case "supplier_invoice":
      return "/admin/payables";
    case "purchase_order":
      return `/admin/purchase-orders/${id}`;
    case "expense":
      return "/admin/expenses";
    case "product":
      return `/admin/products/${id}`;
    case "case":
      return `/admin/cases?case=${id}`;
    case "day_close":
      return "/admin/end-of-day";
    case "supplier_return":
      return `/admin/supplier-returns/${id}`;
    case "requisition":
      return `/admin/requisitions/${id}`;
    case "customer":
      return `/admin/customers/${id}`;
    default:
      return "/admin/promotions";
  }
}

const SOURCE_LABEL: Record<LibraryRow["source"], () => string> = {
  upload: () => t("Added in the library"),
  expense_attachment: () => t("From an expense"),
  invoice_scan: () => t("From a supplier document scan"),
  case_evidence: () => t("From a case"),
};

function statusChip(s: LibraryRow["status"]) {
  if (s === "active") return <Chip tone="success">{t("Current")}</Chip>;
  if (s === "replaced") return <Chip>{t("Older version")}</Chip>;
  return <Chip tone="warning">{t("Archived")}</Chip>;
}

export function textStatusLabel(s: LibraryTextStatus): string {
  if (s === "extracted") return t("Text read");
  if (s === "pending") return t("The text has not been read yet.");
  return t("Text could not be extracted.");
}

/** A search snippet: matched words in [brackets] are highlighted. Plain text only. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/(\[[^\]]*\])/g);
  return (
    <span dir="auto" className="tiny">
      {parts.map((p, i) =>
        p.startsWith("[") && p.endsWith("]") ? <mark key={i}>{p.slice(1, -1)}</mark> : <span key={i}>{p}</span>,
      )}
    </span>
  );
}

type StatusTab = "active" | "archived" | "replaced" | "all";

export function LibraryPage() {
  const [params, setParams] = useSearchParams();
  const { has } = useSession();
  const [tab, setTab] = useState<StatusTab>("active");
  const [category, setCategory] = useState("");
  const [q, setQ] = useState("");
  const [query, setQuery] = useState("");
  const [adding, setAdding] = useState(false);
  const list = useLoad(
    () => api.library.list({ status: tab, category: category || null, limit: 200 }),
    [tab, category],
  );
  const hits = useLoad(
    () =>
      query.trim() ? api.library.search(query, tab !== "active") : Promise.resolve({ results: [] as LibraryHit[] }),
    [query, tab],
  );
  const open = params.get("doc");
  const setOpen = (id: string | null) => {
    const p = new URLSearchParams(params);
    if (id) p.set("doc", id);
    else p.delete("doc");
    setParams(p, { replace: true });
  };
  const counts = list.data?.counts ?? {};
  const searching = query.trim().length > 0;
  return (
    <div>
      <PageHeader
        title={t("Document Library")}
        subtitle={t(
          "Invoices, receipts, contracts and licences, kept with the records they are evidence for. Each file is kept once and never changes; search works on this computer without the assistant.",
        )}
        actions={
          has("documents.manage") ? (
            <Button
              variant="primary"
              icon={<Upload size={18} />}
              onClick={() => setAdding(true)}
              data-testid="library-add"
            >
              {t("Add document")}
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
          placeholder={t("Search the words in your documents")}
          aria-label={t("Search")}
          data-testid="library-search"
          style={{ minWidth: 320 }}
        />
        <Button type="submit">{t("Search")}</Button>
        <select
          className="select"
          style={{ width: "auto" }}
          aria-label={t("Category")}
          value={category}
          onChange={(e) => setCategory(e.target.value)}
        >
          <option value="">{t("Every category")}</option>
          {CATEGORIES.map((c) => (
            <option key={c} value={c}>
              {CATEGORY_LABEL[c]()}
            </option>
          ))}
        </select>
      </form>
      <div style={{ marginTop: 12 }}>
        <Tabs<StatusTab>
          value={tab}
          onChange={setTab}
          tabs={[
            { key: "active", label: `${t("Current")} (${counts.active ?? 0})` },
            { key: "archived", label: `${t("Archived")} (${counts.archived ?? 0})` },
            { key: "replaced", label: `${t("Older versions")} (${counts.replaced ?? 0})` },
            { key: "all", label: t("All") },
          ]}
        />
      </div>
      <div style={{ marginTop: 16 }} data-testid="library-list">
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        {searching ? (
          hits.loading && !hits.data ? (
            <Skeleton rows={4} />
          ) : hits.error ? (
            <Banner tone="danger">{hits.error}</Banner>
          ) : (hits.data?.results.length ?? 0) === 0 ? (
            <Empty title={t("No document mentions this")}>
              {t("Search looks at titles and the text read from each file. Photos are read when text reading is on.")}
            </Empty>
          ) : (
            <div className="col gap-8" data-testid="library-hits">
              {hits.data!.results.map((h, i) => (
                <button
                  key={`${h.document_id}-${h.page ?? 0}-${i}`}
                  className="card card-pad col gap-4 text-start"
                  onClick={() => setOpen(h.document_id)}
                  data-testid="library-hit"
                >
                  <div className="row wrap gap-8">
                    <strong dir="auto">{h.title}</strong>
                    <span className="tiny" dir="ltr">
                      {h.number}
                    </span>
                    <Chip>{CATEGORY_LABEL[h.category]?.() ?? h.category}</Chip>
                    {h.page ? <Chip tone="info">{t("Page {0}", h.page)}</Chip> : null}
                  </div>
                  {h.snippet ? <Snippet text={h.snippet} /> : null}
                </button>
              ))}
            </div>
          )
        ) : (
          <DataTable<LibraryRow>
            rows={list.data?.rows ?? null}
            loading={list.loading}
            rowKey={(r) => r.document_id}
            onRowClick={(r) => setOpen(r.document_id)}
            empty={
              tab === "active" && !category ? (
                <Empty title={t("No documents yet")}>
                  {t(
                    "Add supplier invoices, receipts, contracts and licences here, or attach them to an expense or a case. Receipts and supplier documents added before appear here by themselves.",
                  )}
                </Empty>
              ) : (
                <Empty title={t("Nothing here")} />
              )
            }
            columns={[
              { key: "n", label: t("Document"), render: (r) => <span dir="ltr">{r.number}</span> },
              {
                key: "t",
                label: t("Title"),
                render: (r) => (
                  <div className="col">
                    <span dir="auto">{r.title}</span>
                    <span className="tiny muted">{SOURCE_LABEL[r.source]?.()}</span>
                  </div>
                ),
              },
              { key: "c", label: t("Category"), render: (r) => CATEGORY_LABEL[r.category]?.() ?? r.category },
              {
                key: "d",
                label: t("Date on it"),
                render: (r) => (r.document_date ? formatDate(r.document_date) : "—"),
              },
              { key: "l", label: t("Linked to"), render: (r) => (r.links ? String(r.links) : "—") },
              {
                key: "x",
                label: t("Text"),
                render: (r) =>
                  r.text_status === "extracted" ? (
                    <Chip tone="success">{t("Text read")}</Chip>
                  ) : r.text_status === "pending" ? (
                    <Chip>{t("Not read yet")}</Chip>
                  ) : (
                    <Chip tone="warning">{t("No text")}</Chip>
                  ),
              },
              { key: "s", label: t("Status"), render: (r) => statusChip(r.status) },
              { key: "a", label: t("Added"), render: (r) => formatDateTime(r.added_at), sort: (r) => r.added_at },
            ]}
          />
        )}
      </div>
      {adding ? (
        <AddDocument
          onClose={() => setAdding(false)}
          onAdded={(id) => {
            setAdding(false);
            void list.reload();
            setOpen(id);
          }}
        />
      ) : null}
      {open ? <DocumentDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
    </div>
  );
}

/** Add a document, optionally as evidence for one record. */
export function AddDocument({
  link,
  defaultCategory,
  onClose,
  onAdded,
}: {
  link?: { entity_type: LibraryEntityType; entity_id: string };
  defaultCategory?: LibraryCategory;
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const toast = useToast();
  const act = useAction();
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [category, setCategory] = useState<LibraryCategory>(defaultCategory ?? "other");
  const [date, setDate] = useState("");
  const [note, setNote] = useState("");
  const save = async () => {
    if (!file) return;
    const input: LibraryAddInput = {
      file_name: file.name,
      data_base64: await fileToBase64(file),
      title: title.trim() || null,
      category,
      document_date: date || null,
      note: note.trim() || null,
      links: link ? [link] : [],
    };
    const r = await act.run(() => api.library.add(input));
    if (r) {
      toast(
        "success",
        r.duplicate ? t("This exact file is already in the library as {0}. It is linked there.", r.number) : t("Saved"),
      );
      onAdded(r.document_id);
    }
  };
  return (
    <Modal
      title={t("Add document")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" className="right" disabled={!file} loading={act.busy} onClick={save}>
            {t("Add")}
          </Button>
        </>
      }
    >
      <div className="col gap-12" data-testid="library-add-form">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <Field label={t("File (PDF or photo, up to 20 MB)")}>
          <input
            type="file"
            accept=".pdf,.png,.jpg,.jpeg,.webp,.bmp,.tif,.tiff"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
            data-testid="library-file"
          />
        </Field>
        <TextInput
          label={t("Title")}
          hint={t("Leave empty to use the file name.")}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          maxLength={120}
        />
        <Field label={t("Category")}>
          <select className="select" value={category} onChange={(e) => setCategory(e.target.value as LibraryCategory)}>
            {CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {CATEGORY_LABEL[c]()}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Date on the document")} hint={t("Optional. Only what is printed on it.")}>
          <input type="date" className="input" value={date} onChange={(e) => setDate(e.target.value)} />
        </Field>
        <TextInput label={t("Note")} value={note} onChange={(e) => setNote(e.target.value)} maxLength={500} />
      </div>
    </Modal>
  );
}

function PageText({ doc }: { doc: LibraryDocument }) {
  const pages = doc.text_pages;
  const [page, setPage] = useState<number | null>(pages[0] ?? null);
  const text = useLoad(
    () => (pages.length ? api.library.text(doc.document.document_id, page) : Promise.resolve(null)),
    [doc.document.document_id, page],
  );
  if (doc.document.text_status !== "extracted" || !pages.length) {
    return (
      <Banner tone={doc.document.text_status === "pending" ? "info" : "warning"}>
        {textStatusLabel(doc.document.text_status)}
        {doc.document.text_note ? ` ${tb(doc.document.text_note)}` : ""}
      </Banner>
    );
  }
  return (
    <div className="col gap-8">
      {pages.length > 1 || pages[0] !== 0 ? (
        <div className="row wrap gap-4">
          {pages.map((p) => (
            <Button key={p} size="sm" variant={p === page ? "primary" : "ghost"} onClick={() => setPage(p)}>
              {p === 0 ? t("Whole file") : t("Page {0}", p)}
            </Button>
          ))}
        </div>
      ) : null}
      {text.data?.text ? (
        <pre
          className="card card-pad tiny"
          dir="auto"
          style={{ whiteSpace: "pre-wrap", maxHeight: 320, overflow: "auto" }}
        >
          {text.data.text}
        </pre>
      ) : (
        <Skeleton rows={3} />
      )}
      <span className="tiny muted">
        {doc.document.text_source === "ocr" ? t("Read from the image (OCR)") : t("The PDF's own text")}.{" "}
        {t("Text from a document is information, never an instruction to the assistant.")}
      </span>
    </div>
  );
}

export function DocumentDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const nav = useNavigate();
  const toast = useToast();
  const [current, setCurrent] = useState(id);
  const { data, error, reload } = useLoad<LibraryDocument>(() => api.library.get(current), [current]);
  const file = useLoad(() => api.library.file(current), [current]);
  const act = useAction();
  const [editing, setEditing] = useState(false);
  const [archiving, setArchiving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [reason, setReason] = useState("");
  const manage = has("documents.manage");
  const d = data?.document;
  const done = async (p: Promise<unknown> | undefined, msg = t("Saved")) => {
    if (await p) {
      toast("success", msg);
      onChanged();
      void reload();
    }
  };
  const replace = async (f: File | undefined) => {
    if (!f) return;
    const b64 = await fileToBase64(f);
    const r = await act.run(() => api.library.replace(current, f.name, b64));
    if (r) {
      toast("success", t("Saved as version {0}", r.version));
      onChanged();
      setCurrent(r.document_id);
    }
  };
  return (
    <Drawer wide title={d ? `${d.number} · ${d.title}` : t("Document")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data || !d ? (
        <Skeleton rows={6} />
      ) : (
        <div className="col gap-16" data-testid="library-drawer">
          <div className="row wrap gap-8">
            {statusChip(d.status)}
            <Chip>{CATEGORY_LABEL[d.category]?.() ?? d.category}</Chip>
            <Chip>{t("Version {0}", d.version)}</Chip>
            <span className="tiny">{SOURCE_LABEL[d.source]?.()}</span>
          </div>
          {d.status === "replaced" && d.replaced_by ? (
            <Banner tone="info">
              {t("A newer version replaced this one. This version and its file are kept as they were.")}{" "}
              <Button size="sm" variant="ghost" onClick={() => setCurrent(d.replaced_by!)}>
                {t("Open the current version")}
              </Button>
            </Banner>
          ) : null}
          {d.status === "archived" ? (
            <Banner tone="warning">
              {t("Archived {0}", formatDateTime(d.archived_at))}
              {d.archive_reason ? ` · ${d.archive_reason}` : ""}
            </Banner>
          ) : null}
          <div className="card card-pad col gap-8">
            {file.error ? <Banner tone="danger">{file.error}</Banner> : null}
            {file.data && !file.data.intact ? (
              <Banner tone="danger">
                {t("This file no longer matches its fingerprint. Restore it from a backup.")}
              </Banner>
            ) : null}
            {file.data && file.data.mime.startsWith("image/") ? (
              <img
                alt={d.title}
                src={`data:${file.data.mime};base64,${file.data.base64}`}
                style={{ maxWidth: "100%", maxHeight: 360, objectFit: "contain" }}
              />
            ) : null}
            <div className="row wrap gap-8">
              <Button
                icon={<FileText size={18} />}
                disabled={!file.data}
                onClick={() => file.data && downloadBase64(file.data.file_name, file.data.base64, file.data.mime)}
                data-testid="library-open-file"
              >
                {t("Save a copy")}
              </Button>
              {manage && d.status === "active" ? (
                <label className="btn">
                  <Upload size={18} /> {t("Replace with a new version")}
                  <input
                    type="file"
                    hidden
                    accept=".pdf,.png,.jpg,.jpeg,.webp,.bmp,.tif,.tiff"
                    onChange={(e) => void replace(e.target.files?.[0])}
                  />
                </label>
              ) : null}
            </div>
          </div>
          <table className="table">
            <tbody>
              <tr>
                <td className="tiny">{t("File")}</td>
                <td dir="auto">{d.original_name}</td>
              </tr>
              <tr>
                <td className="tiny">{t("Date on it")}</td>
                <td>{d.document_date ? formatDate(d.document_date) : t("Not entered")}</td>
              </tr>
              <tr>
                <td className="tiny">{t("Added")}</td>
                <td>
                  {formatDateTime(d.added_at)} · {d.added_by_name ?? ""}
                </td>
              </tr>
              {d.note ? (
                <tr>
                  <td className="tiny">{t("Note")}</td>
                  <td dir="auto">{d.note}</td>
                </tr>
              ) : null}
              <tr>
                <td className="tiny">{t("Fingerprint (SHA-256)")}</td>
                <td className="mono tiny" dir="ltr">
                  {d.sha256.slice(0, 16)}…
                </td>
              </tr>
            </tbody>
          </table>
          <div className="card card-pad col gap-8">
            <h3>{t("Evidence for")}</h3>
            {data.links.length === 0 ? (
              <span className="muted">{t("Not linked to any record. Link it from the record's screen.")}</span>
            ) : (
              data.links.map((l) => (
                <div key={l.link_id} className="row wrap gap-8" data-testid="library-link">
                  <Chip>{ENTITY_LABEL[l.entity_type]?.() ?? l.entity_type}</Chip>
                  {l.exists ? (
                    <Button size="sm" variant="ghost" onClick={() => nav(entityLink(l.entity_type, l.entity_id))}>
                      <span dir="auto">{l.label ?? l.entity_id}</span>
                    </Button>
                  ) : (
                    <span className="muted">{t("The record no longer exists.")}</span>
                  )}
                  {manage && d.status === "active" ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      className="right"
                      onClick={() => void done(act.run(() => api.library.unlink(current, l.link_id)))}
                    >
                      {t("Remove link")}
                    </Button>
                  ) : null}
                </div>
              ))
            )}
          </div>
          <div className="card card-pad col gap-8">
            <h3>{t("Text read from the file")}</h3>
            <PageText doc={data} />
          </div>
          {data.versions.length > 1 ? (
            <div className="card card-pad col gap-8">
              <h3>{t("Versions")}</h3>
              {data.versions.map((v) => (
                <div key={v.document_id} className="row gap-8">
                  <Button
                    size="sm"
                    variant={v.document_id === current ? "primary" : "ghost"}
                    onClick={() => setCurrent(v.document_id)}
                  >
                    {t("Version {0}", v.version)} · <span dir="ltr">{v.number}</span>
                  </Button>
                  {statusChip(v.status as LibraryRow["status"])}
                </div>
              ))}
            </div>
          ) : null}
          {manage ? (
            <div className="row wrap gap-8">
              {d.status === "active" ? (
                <>
                  <Button onClick={() => setEditing(true)}>{t("Edit details")}</Button>
                  <Button onClick={() => setArchiving(true)} data-testid="library-archive">
                    {t("Archive")}
                  </Button>
                </>
              ) : null}
              {d.status === "archived" ? (
                <Button onClick={() => void done(act.run(() => api.library.unarchive(current)))}>{t("Restore")}</Button>
              ) : null}
              {data.can_delete ? (
                <Button variant="danger" onClick={() => setDeleting(true)}>
                  {t("Delete")}
                </Button>
              ) : null}
            </div>
          ) : null}
        </div>
      )}
      {editing && d ? (
        <EditDetails
          doc={d}
          onClose={() => setEditing(false)}
          onSaved={() => {
            setEditing(false);
            onChanged();
            void reload();
          }}
        />
      ) : null}
      {archiving ? (
        <Confirm
          title={t("Archive this document?")}
          confirmLabel={t("Archive")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setArchiving(false)}
          onConfirm={async () => {
            if (await act.run(() => api.library.archive(current, reason))) {
              setArchiving(false);
              setReason("");
              onChanged();
              void reload();
            }
          }}
        >
          <div className="col gap-8">
            <span>
              {t(
                "It leaves the current list but is kept, with its file and links, and can be restored. Nothing is deleted.",
              )}
            </span>
            <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} maxLength={300} />
          </div>
        </Confirm>
      ) : null}
      {deleting ? (
        <Confirm
          title={t("Delete this document?")}
          confirmLabel={t("Delete")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setDeleting(false)}
          onConfirm={async () => {
            if (await act.run(() => api.library.remove(current))) {
              setDeleting(false);
              onChanged();
              onClose();
            }
          }}
        >
          {t("It was never evidence for a record, so it can be deleted. The deletion is recorded in the audit trail.")}
        </Confirm>
      ) : null}
    </Drawer>
  );
}

function EditDetails({
  doc,
  onClose,
  onSaved,
}: {
  doc: LibraryDocument["document"];
  onClose: () => void;
  onSaved: () => void;
}) {
  const act = useAction();
  const [title, setTitle] = useState(doc.title);
  const [category, setCategory] = useState<LibraryCategory>(doc.category);
  const [date, setDate] = useState(doc.document_date ?? "");
  const [note, setNote] = useState(doc.note ?? "");
  return (
    <Modal
      title={t("Edit details")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            onClick={async () => {
              if (
                await act.run(() => api.library.update(doc.document_id, { title, category, document_date: date, note }))
              )
                onSaved();
            }}
          >
            {t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <span className="tiny muted">{t("The file itself cannot be changed. To change it, add a new version.")}</span>
        <TextInput label={t("Title")} value={title} onChange={(e) => setTitle(e.target.value)} maxLength={120} />
        <Field label={t("Category")}>
          <select className="select" value={category} onChange={(e) => setCategory(e.target.value as LibraryCategory)}>
            {CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {CATEGORY_LABEL[c]()}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Date on the document")}>
          <input type="date" className="input" value={date} onChange={(e) => setDate(e.target.value)} />
        </Field>
        <TextInput label={t("Note")} value={note} onChange={(e) => setNote(e.target.value)} maxLength={500} />
      </div>
    </Modal>
  );
}

/** Documents that are evidence for one record, on that record's screen. */
export function LinkedDocuments({
  entityType,
  entityId,
  defaultCategory,
}: {
  entityType: LibraryEntityType;
  entityId: string;
  defaultCategory?: LibraryCategory;
}) {
  const { has } = useSession();
  const [open, setOpen] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const list = useLoad(
    () =>
      has("documents.view")
        ? api.library.list({ entity_type: entityType, entity_id: entityId, status: "active" })
        : Promise.resolve(null),
    [entityType, entityId],
  );
  if (!has("documents.view")) return null;
  return (
    <div className="card card-pad col gap-8" data-testid="linked-documents">
      <div className="row">
        <h3 className="grow">{t("Documents")}</h3>
        {has("documents.manage") ? (
          <Button size="sm" icon={<Upload size={16} />} onClick={() => setAdding(true)}>
            {t("Add document")}
          </Button>
        ) : null}
      </div>
      {list.error ? <span className="muted tiny">{list.error}</span> : null}
      {list.data && list.data.rows.length === 0 ? (
        <span className="muted">{t("No documents for this record.")}</span>
      ) : null}
      {list.data?.rows.map((r) => (
        <button key={r.document_id} className="row gap-8 text-start link-row" onClick={() => setOpen(r.document_id)}>
          <FileText size={16} />
          <span dir="auto" className="grow">
            {r.title}
          </span>
          <span className="tiny">{CATEGORY_LABEL[r.category]?.()}</span>
        </button>
      ))}
      {adding ? (
        <AddDocument
          link={{ entity_type: entityType, entity_id: entityId }}
          defaultCategory={defaultCategory}
          onClose={() => setAdding(false)}
          onAdded={() => {
            setAdding(false);
            void list.reload();
          }}
        />
      ) : null}
      {open ? <DocumentDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
    </div>
  );
}
