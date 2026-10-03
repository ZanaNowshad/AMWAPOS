// Supplier document review: the original page with the evidence of each value,
// extracted header fields and lines with confidence bands, deterministic
// checks, duplicates, anomaly signals and PO reconciliation. A person corrects
// what is wrong and explicitly creates a supplier invoice record or a receiving
// draft; stock moves only when a person with receiving rights posts the draft.
import { useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, FileCheck2, PackagePlus, Sparkles } from "lucide-react";
import { api } from "../../api";
import type { Band, DocEvidence, DocField, DocLine, ReceivingDraft, SupplierInvoice } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate, useFeature } from "../../components/FeatureGate";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";
import { formatAmount, formatMoney, formatQty, parseMoney, parseQty } from "../../lib/money";
import { formatDateTime } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";
import { InlineNumber, ProductPick } from "./automation";

const BAND_TONE: Record<Band, "success" | "info" | "warning" | "danger"> = {
  high: "success",
  medium: "info",
  low: "warning",
  unresolved: "danger",
};
const BAND_LABEL: Record<Band, () => string> = {
  high: () => t("High confidence"),
  medium: () => t("Medium confidence"),
  low: () => t("Low confidence"),
  unresolved: () => t("Unresolved"),
};

export function BandChip({ band }: { band: Band | null | undefined }) {
  const b = band ?? "unresolved";
  return <Chip tone={BAND_TONE[b]}>{BAND_LABEL[b]()}</Chip>;
}

const SOURCE_LABEL: Record<string, () => string> = {
  rules: () => t("Read by rules"),
  ai: () => t("Read by AI"),
  person: () => t("Corrected by a person"),
  learned: () => t("Learned from a correction"),
};

const DOC_TYPE_LABEL: Record<string, () => string> = {
  invoice: () => t("Invoice"),
  credit_note: () => t("Credit note"),
  delivery_note: () => t("Delivery note"),
  unknown: () => t("Unknown document"),
};

const STAGE_LABEL: Record<string, () => string> = {
  uploaded: () => t("Uploaded"),
  preprocessing: () => t("Preparing the pages"),
  ocr: () => t("Reading the text"),
  extracting: () => t("Extracting fields and lines"),
  matching: () => t("Matching supplier and products"),
  validating: () => t("Checking totals and VAT"),
  ready_for_review: () => t("Ready for review"),
  failed: () => t("Failed"),
};

const FIELD_LABEL: [string, () => string, "text" | "date" | "money" | "rate" | "plain"][] = [
  ["supplier_name", () => t("Supplier name"), "plain"],
  ["supplier_vat", () => t("Supplier VAT number"), "plain"],
  ["supplier_cr", () => t("Supplier CR number"), "plain"],
  ["supplier_phone", () => t("Supplier phone"), "plain"],
  ["invoice_number", () => t("Invoice number"), "text"],
  ["invoice_date", () => t("Invoice date"), "date"],
  ["due_date", () => t("Due date"), "date"],
  ["delivery_date", () => t("Delivery date"), "plain"],
  ["po_number", () => t("PO number"), "plain"],
  ["buyer_vat", () => t("Buyer VAT number"), "plain"],
  ["subtotal_minor", () => t("Subtotal"), "money"],
  ["discount_minor", () => t("Discount"), "money"],
  ["vat_rate_bp", () => t("VAT rate"), "rate"],
  ["vat_minor", () => t("VAT"), "money"],
  ["exempt_minor", () => t("Exempt amount"), "money"],
  ["zero_rated_minor", () => t("Zero-rated amount"), "money"],
  ["total_minor", () => t("Grand total"), "money"],
  ["iban", () => t("IBAN"), "plain"],
];
const EDITABLE: Record<string, string> = {
  invoice_number: "invoice_number",
  invoice_date: "invoice_date",
  due_date: "due_date",
  subtotal_minor: "subtotal_minor",
  vat_minor: "vat_minor",
  total_minor: "total_minor",
};

const RECON_LABEL: Record<string, () => string> = {
  fully_matched: () => t("Matches the order"),
  quantity_variance: () => t("Quantity differs from the order"),
  cost_variance: () => t("Cost differs from the order"),
  invoice_exceeds_received: () => t("Invoiced more than received"),
  missing_from_invoice: () => t("Ordered but not on the invoice"),
  not_on_po: () => t("Not on the order"),
  duplicate_line: () => t("Duplicate line"),
};

const DUP_LABEL: Record<string, () => string> = {
  exact_document: () => t("Same file uploaded before"),
  same_invoice: () => t("Same supplier invoice number"),
  same_scanned_differently: () => t("Same invoice scanned differently"),
  possible_duplicate: () => t("Possible duplicate"),
};

const MATCH_KIND: Record<string, () => string> = {
  barcode: () => t("Barcode"),
  supplier_map: () => t("Learned supplier code"),
  supplier_code: () => t("Supplier code"),
  sku: () => t("SKU"),
  name: () => t("Exact name"),
  fuzzy: () => t("Similar name"),
  ai: () => t("AI suggestion"),
  manual: () => t("Chosen by a person"),
  none: () => t("No match"),
};

function fieldText(f: DocField, kind: string): string {
  if (f.value === null || f.value === undefined || f.value === "") return "—";
  if (kind === "money" && typeof f.value === "number") return formatMoney(f.value);
  if (kind === "rate" && typeof f.value === "number") return `${(f.value / 100).toFixed(2)}%`;
  return String(f.value);
}

/** The document page with the evidence box of the selected value. */
function PageView({ id, pages, focus }: { id: string; pages: number; focus: DocEvidence | null }) {
  const [page, setPage] = useState(1);
  useEffect(() => {
    if (focus?.page) setPage(focus.page);
  }, [focus]);
  const img = useLoad(() => api.docs.page(id, page), [id, page]);
  const box = focus?.bbox && (focus.page ?? 1) === page ? focus.bbox : null;
  return (
    <div className="card card-pad col gap-8 doc-page" data-testid="doc-page">
      {pages > 1 ? (
        <div className="row">
          <Button size="sm" disabled={page <= 1} onClick={() => setPage(page - 1)}>
            {t("Previous page")}
          </Button>
          <span className="small">{t("Page {0} of {1}", page, pages)}</span>
          <Button size="sm" disabled={page >= pages} onClick={() => setPage(page + 1)}>
            {t("Next page")}
          </Button>
        </div>
      ) : null}
      {img.data?.image ? (
        <div className="doc-image">
          <img src={`data:${img.data.image.mime};base64,${img.data.image.base64}`} alt={t("Document page {0}", page)} />
          {box ? (
            <div
              className="doc-box"
              data-testid="doc-evidence-box"
              style={{
                left: `${box[0] * 100}%`,
                top: `${box[1] * 100}%`,
                width: `${box[2] * 100}%`,
                height: `${box[3] * 100}%`,
              }}
            />
          ) : null}
        </div>
      ) : img.loading ? (
        <Skeleton rows={3} />
      ) : (
        <div className="tiny">{t("No page image is available for this document.")}</div>
      )}
      {focus?.text ? (
        <div className="small" dir="auto">
          {t("Printed text")}: <strong>{focus.text}</strong>
          {focus.ocr_conf !== null ? ` · ${t("OCR confidence")} ${focus.ocr_conf}%` : ""}
        </div>
      ) : null}
    </div>
  );
}

export function DocumentReviewPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const { data, error, setData, reload } = useLoad(() => api.docs.get(id), [id]);
  const act = useAction();
  const [focus, setFocus] = useState<DocEvidence | null>(null);
  const [picking, setPicking] = useState<DocLine | null>(null);
  const [reject, setReject] = useState(false);
  const [reason, setReason] = useState("");
  const aiOn = useFeature("ocr.ai_parse");
  const busyStage = data && !["ready_for_review", "failed"].includes(data.stage) && data.scan.status === "imported";
  useEffect(() => {
    if (!busyStage) return;
    const iv = window.setInterval(() => void reload(), 2500);
    return () => window.clearInterval(iv);
  }, [busyStage, reload]);

  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const s = data.scan;
  const editable = s.status === "review" && has("ocr.scan");
  const canDraft = has("purchasing.manage") && s.status === "review";
  const f = data.fields ?? {};
  const v = data.validation;

  const patch = async (p: Record<string, unknown>) => {
    const r = await act.run(() => api.docs.update(id, { revision: data.revision, ...p }));
    if (r) setData(r);
  };
  const patchLine = async (l: DocLine, p: Record<string, unknown>) => {
    const r = await act.run(() => api.docs.updateLine(id, { revision: data.revision, line_no: l.line_no, ...p }));
    if (r) setData(r);
  };

  return (
    <div>
      <PageHeader
        title={`${DOC_TYPE_LABEL[data.classification.doc_type]?.() ?? data.classification.doc_type} ${s.scan_number}`}
        subtitle={data.summary ?? undefined}
        actions={
          <Button icon={<ArrowLeft size={16} />} onClick={() => navigate("/admin/invoice-scan")}>
            {t("All documents")}
          </Button>
        }
      />
      <FeatureGate feature="ocr.supplier_invoices">
        <div className="col gap-16">
          <div className="row" style={{ flexWrap: "wrap" }}>
            <Chip tone={data.stage === "failed" ? "danger" : data.stage === "ready_for_review" ? "success" : "info"}>
              {STAGE_LABEL[data.stage]?.() ?? data.stage}
            </Chip>
            <Chip>{DOC_TYPE_LABEL[data.classification.doc_type]?.() ?? data.classification.doc_type}</Chip>
            <BandChip band={data.classification.band} />
            {data.page_count && data.page_count > 1 ? <Chip>{t("{0} pages", data.page_count)}</Chip> : null}
            {data.source === "whatsapp" ? <Chip tone="brand">{t("From WhatsApp")}</Chip> : null}
            {data.ai_model ? <Chip tone="brand">{t("AI assisted")}</Chip> : null}
            {data.corrections ? <Chip>{t("{0} corrections", data.corrections)}</Chip> : null}
          </div>
          {busyStage ? <Banner tone="info">{t("Reading the document… this can take up to a minute.")}</Banner> : null}
          {s.status === "failed" ? (
            <Banner
              tone="danger"
              action={
                <Button
                  onClick={async () => (await act.run(() => api.ocr.retry("invoice", id))) !== undefined && reload()}
                >
                  {t("Read again")}
                </Button>
              }
            >
              {s.error ? tb(s.error) : t("OCR failed")}
            </Banner>
          ) : null}
          {data.classification.doc_type === "credit_note" ? (
            <Banner tone="warning">
              {t("This is a credit note. It never creates a receiving draft; record it as a supplier document only.")}
            </Banner>
          ) : null}
          {data.classification.doc_type === "unknown" && s.status === "review" ? (
            <Banner tone="warning">
              {t("The document type is unclear. Choose the type before creating anything from it.")}
            </Banner>
          ) : null}
          {(data.quality.messages ?? []).length ? (
            <Banner tone="warning" title={t("Image quality")}>
              <ul className="plain-list">
                {(data.quality.messages ?? []).map((m, i) => (
                  <li key={i}>{tb(m)}</li>
                ))}
              </ul>
            </Banner>
          ) : null}
          {(data.duplicates ?? []).length ? (
            <Banner tone="warning" title={t("Possible duplicate")}>
              <ul className="plain-list" data-testid="doc-duplicates">
                {(data.duplicates ?? []).map((d) => (
                  <li key={d.scan_id}>
                    {DUP_LABEL[d.kind]?.() ?? d.kind}:{" "}
                    <Link to={`/admin/invoice-scan/${d.scan_id}`}>{d.scan_number}</Link> — {d.reasons.join("; ")}
                  </li>
                ))}
              </ul>
            </Banner>
          ) : null}

          <div className="doc-grid">
            <PageView id={id} pages={data.page_count ?? 1} focus={focus} />
            <div className="col gap-16">
              <div className="card card-pad col gap-8">
                <h3>{t("Document")}</h3>
                <div className="row" style={{ alignItems: "flex-end", flexWrap: "wrap" }}>
                  <Field label={t("Document type")}>
                    <select
                      className="select"
                      disabled={!editable}
                      value={data.classification.doc_type}
                      aria-label={t("Document type")}
                      onChange={(e) => void patch({ doc_type: e.target.value })}
                    >
                      {Object.keys(DOC_TYPE_LABEL).map((k) => (
                        <option key={k} value={k}>
                          {DOC_TYPE_LABEL[k]()}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <SupplierField
                    value={s.supplier_id ?? ""}
                    disabled={!editable}
                    onChange={(sid) => void patch({ supplier_id: sid })}
                  />
                </div>
                {data.supplier_match ? (
                  <div className="small">
                    <BandChip band={data.supplier_match.band} /> {data.supplier_match.reasons.join("; ")}
                    {data.supplier_match.alternatives.length ? (
                      <div className="tiny">
                        {t("Other possible suppliers")}:{" "}
                        {data.supplier_match.alternatives.map((a) => `${a.name} (${a.score}%)`).join(", ")}
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </div>

              <div className="card">
                <div className="table-wrap">
                  <table className="table" data-testid="doc-fields">
                    <thead>
                      <tr>
                        <th>{t("Field")}</th>
                        <th>{t("Value")}</th>
                        <th>{t("Confidence")}</th>
                        <th>{t("Source")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {FIELD_LABEL.filter(([k]) => f[k]).map(([k, label, kind]) => {
                        const fl = f[k];
                        if (fl.status === "missing" && !EDITABLE[k] && k !== "supplier_vat") return null;
                        return (
                          <tr
                            key={k}
                            className={fl.evidence ? "clickable" : ""}
                            onClick={() => fl.evidence && setFocus(fl.evidence)}
                          >
                            <td>{label()}</td>
                            <td dir="auto">
                              {editable && EDITABLE[k] ? (
                                <FieldEditor
                                  kind={kind}
                                  field={fl}
                                  label={label()}
                                  onCommit={(val) => void patch({ [EDITABLE[k]]: val })}
                                />
                              ) : (
                                fieldText(fl, kind)
                              )}
                              {fl.note ? <div className="tiny">{tb(fl.note)}</div> : null}
                            </td>
                            <td>
                              <BandChip band={fl.band} />
                              {fl.status === "conflict" ? <Chip tone="warning">{t("Conflict")}</Chip> : null}
                            </td>
                            <td className="tiny">{SOURCE_LABEL[fl.source]?.() ?? fl.source}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </div>

              {v ? (
                <div className="card card-pad col gap-8" data-testid="doc-validation">
                  <h3>{t("Checks")}</h3>
                  <div className="row" style={{ flexWrap: "wrap" }}>
                    <Chip tone={v.arithmetic_ok ? "success" : "danger"}>
                      {v.arithmetic_ok ? t("Totals add up") : t("Totals do not add up")}
                    </Chip>
                    <Chip tone={v.vat_ok ? "success" : "danger"}>
                      {v.vat_ok ? t("VAT checks out") : t("VAT does not check out")}
                    </Chip>
                    <Chip>{v.line_basis === "incl" ? t("Line prices include VAT") : t("Line prices exclude VAT")}</Chip>
                  </div>
                  {v.issues.length ? (
                    <ul className="plain-list">
                      {v.issues.map((i, n) => (
                        <li key={n} className={i.severity === "error" ? "neg-num" : ""}>
                          {tb(i.message)}
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </div>
              ) : null}
              {(data.anomalies ?? []).length ? (
                <div className="card card-pad col gap-8" data-testid="doc-anomalies">
                  <h3>{t("Document anomaly signals")}</h3>
                  <div className="tiny">{t("Reasons to look closer, not findings of fraud. A person decides.")}</div>
                  <ul className="plain-list">
                    {(data.anomalies ?? []).map((a, n) => (
                      <li key={n}>{tb(a.message)}</li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </div>
          </div>

          <div className="card">
            <div className="table-wrap">
              <table className="table" data-testid="doc-lines">
                <thead>
                  <tr>
                    <th title={t("Receive this line (it stays in the document checks either way)")}>{t("Use")}</th>
                    <th>{t("Printed line")}</th>
                    <th>{t("Product")}</th>
                    <th className="num">{t("Qty")}</th>
                    <th className="num">{t("Units per case")}</th>
                    <th className="num">{t("Unit cost")}</th>
                    <th className="num">{t("Line total")}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.lines.map((l) => {
                    const rec = data.receive.find((r) => r.line_no === l.line_no);
                    return (
                      <tr
                        key={l.line_no}
                        className={l.include ? "" : "muted"}
                        data-testid={`doc-line-${l.line_no}`}
                        onClick={() => l.evidence && setFocus(l.evidence)}
                      >
                        <td>
                          <input
                            type="checkbox"
                            aria-label={t("Include line {0}", l.line_no)}
                            disabled={!editable}
                            checked={l.include}
                            onChange={(e) => void patchLine(l, { include: e.target.checked })}
                          />
                        </td>
                        <td className="small" dir="auto">
                          {l.raw_text}
                          {l.barcode ? (
                            <div className="tiny">
                              {t("Barcode")} {l.barcode}
                              {l.barcode_valid === false ? ` · ${t("check digit is wrong")}` : ""}
                            </div>
                          ) : null}
                          {(l.flags ?? []).includes("not_item") ? (
                            <div>
                              <Chip>{t("Not an item")}</Chip>
                            </div>
                          ) : editable && !l.product_id ? (
                            <div>
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={(e) => (e.stopPropagation(), void patchLine(l, { not_item: true }))}
                              >
                                {t("Not an item line")}
                              </Button>
                            </div>
                          ) : null}
                          {l.pack_text ? (
                            <div className="tiny">
                              {t("Pack")} {l.pack_text}
                              {!l.pack_clear ? ` · ${t("pack size unclear")}` : ""}
                            </div>
                          ) : null}
                        </td>
                        <td>
                          <div className="col" style={{ gap: 4 }}>
                            <span>
                              {l.product_name ?? <em className="muted">{t("No match")}</em>}{" "}
                              <BandChip band={l.match_band} />
                            </span>
                            <span className="tiny">
                              {MATCH_KIND[l.match_kind]?.() ?? l.match_kind}
                              {(l.reasons ?? []).length ? ` · ${(l.reasons ?? []).join("; ")}` : ""}
                            </span>
                            {l.last_cost_minor !== null &&
                            l.pack_clear &&
                            rec?.receive_unit_cost_minor !== null &&
                            rec ? (
                              <span className="tiny">
                                {t("Last cost {0}", formatMoney(l.last_cost_minor))} ·{" "}
                                {t("This invoice {0} per unit", formatMoney(rec.receive_unit_cost_minor))}
                              </span>
                            ) : null}
                            {editable && (l.candidates ?? []).length > 1 ? (
                              <select
                                className="select"
                                aria-label={t("Candidates for line {0}", l.line_no)}
                                value={l.product_id ?? ""}
                                onClick={(e) => e.stopPropagation()}
                                onChange={(e) => void patchLine(l, { product_id: e.target.value })}
                              >
                                <option value="">{t("Choose…")}</option>
                                {(l.candidates ?? []).map((c) => (
                                  <option key={c.id} value={c.id}>
                                    {c.name} ({c.score}%)
                                  </option>
                                ))}
                              </select>
                            ) : null}
                            {editable ? (
                              <div className="row" style={{ gap: 4 }}>
                                <Button size="sm" variant="ghost" onClick={(e) => (e.stopPropagation(), setPicking(l))}>
                                  {l.product_id ? t("Change") : t("Choose product")}
                                </Button>
                                {!l.product_id ? (
                                  <Checkbox
                                    label={t("New product")}
                                    checked={l.new_product}
                                    onChange={(x) => void patchLine(l, { new_product: x })}
                                  />
                                ) : null}
                              </div>
                            ) : null}
                          </div>
                        </td>
                        <td className="num">
                          <InlineNumber
                            disabled={!editable}
                            value={l.qty_milli === null ? "" : formatQty(l.qty_milli)}
                            onCommit={(x) => {
                              const q = parseQty(x);
                              if (q !== null) void patchLine(l, { qty_milli: q });
                            }}
                          />
                          <div className="tiny">
                            {l.unit ?? ""}
                            {rec?.receive_qty_milli !== null && rec && rec.receive_qty_milli !== l.qty_milli
                              ? ` = ${formatQty(rec.receive_qty_milli)} ${t("units")}`
                              : ""}
                          </div>
                        </td>
                        <td className="num">
                          <InlineNumber
                            disabled={!editable}
                            value={l.units_per_case ? String(l.units_per_case) : ""}
                            onCommit={(x) => {
                              const n = Number(x.replace(/[^\d]/g, "") || "0");
                              void patchLine(l, { units_per_case: n });
                            }}
                          />
                        </td>
                        <td className="num">
                          <InlineNumber
                            disabled={!editable}
                            value={l.unit_cost_minor === null ? "" : formatAmount(l.unit_cost_minor)}
                            onCommit={(x) => {
                              const c = parseMoney(x);
                              if (c !== null) void patchLine(l, { unit_cost_minor: c });
                            }}
                          />
                        </td>
                        <td className="num">{l.line_total_minor === null ? "—" : formatMoney(l.line_total_minor)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>

          {data.recon && (data.recon.po_id || data.recon.candidates.length) ? (
            <div className="card card-pad col gap-8" data-testid="doc-recon">
              <h3>{data.recon.three_way ? t("Order, receipt and invoice") : t("Purchase order and invoice")}</h3>
              <Field label={t("Purchase order")}>
                <select
                  className="select"
                  disabled={!editable}
                  aria-label={t("Purchase order")}
                  value={data.recon.po_id ?? ""}
                  onChange={(e) => void patch({ po_id: e.target.value })}
                >
                  <option value="">{t("No purchase order")}</option>
                  {data.recon.po_id && !data.recon.candidates.some((c) => c.po_id === data.recon!.po_id) ? (
                    <option value={data.recon.po_id}>{data.recon.po_number}</option>
                  ) : null}
                  {data.recon.candidates.map((c) => (
                    <option key={c.po_id} value={c.po_id}>
                      {c.po_number} ({c.score}%)
                    </option>
                  ))}
                </select>
              </Field>
              {data.recon.summary.map((x, i) => (
                <div key={i} className="small">
                  {tb(x)}
                </div>
              ))}
              {data.recon.lines.length ? (
                <div className="table-wrap">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>{t("Product")}</th>
                        <th className="num">{t("Ordered")}</th>
                        <th className="num">{t("Received")}</th>
                        <th className="num">{t("Invoiced")}</th>
                        <th className="num">{t("Cost variance")}</th>
                        <th>{t("Status")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {data.recon.lines.map((r, i) => (
                        <tr key={i}>
                          <td>{r.product_name ?? "—"}</td>
                          <td className="num">{r.ordered_milli === null ? "—" : formatQty(r.ordered_milli)}</td>
                          <td className="num">{r.received_milli === null ? "—" : formatQty(r.received_milli)}</td>
                          <td className="num">{r.invoiced_milli === null ? "—" : formatQty(r.invoiced_milli)}</td>
                          <td className="num">{r.cost_variance_pct ?? "—"}</td>
                          <td>
                            <div className="col" style={{ gap: 2 }}>
                              {r.states.map((st) => (
                                <Chip key={st} tone={st === "fully_matched" ? "success" : "warning"}>
                                  {RECON_LABEL[st]?.() ?? st}
                                </Chip>
                              ))}
                              {r.notes.map((n, j) => (
                                <span key={j} className="tiny">
                                  {tb(n)}
                                </span>
                              ))}
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : null}
            </div>
          ) : null}

          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          <div className="row" style={{ flexWrap: "wrap" }}>
            {data.supplier_invoice_id ? (
              <Chip tone="success">{t("Supplier invoice record created")}</Chip>
            ) : canDraft && data.classification.doc_type !== "unknown" ? (
              <Button
                icon={<FileCheck2 size={16} />}
                loading={act.busy}
                data-testid="doc-create-invoice"
                onClick={async () => {
                  const r = await act.run(() => api.docs.createSupplierInvoice(id, data.revision));
                  if (r) {
                    toast("success", t("Supplier invoice {0} saved as a draft record", r.number));
                    void reload();
                  }
                }}
              >
                {t("Create supplier invoice record")}
              </Button>
            ) : null}
            {data.receiving_draft_id ? (
              <Chip tone="success">{t("Receiving draft created")}</Chip>
            ) : canDraft && ["invoice", "delivery_note"].includes(data.classification.doc_type) ? (
              <Button
                variant="primary"
                icon={<PackagePlus size={16} />}
                loading={act.busy}
                data-testid="doc-create-receiving"
                onClick={async () => {
                  const r = await act.run(() => api.docs.createReceiving(id, data.revision));
                  if (r) {
                    toast("success", t("Receiving draft {0} created. Stock has not changed.", r.number));
                    void reload();
                  }
                }}
              >
                {t("Create receiving draft")}
              </Button>
            ) : null}
            {aiOn && editable ? (
              <Button
                icon={<Sparkles size={16} />}
                loading={act.busy}
                data-testid="doc-ai"
                onClick={async () => {
                  const r = await act.run(() => api.invoiceScan.aiParse(id));
                  if (r) {
                    toast(
                      r.replaced ? "success" : "info",
                      r.replaced
                        ? t("The AI provider filled unresolved values. Check them again.")
                        : t("The AI found nothing better to add."),
                    );
                    void reload();
                  }
                }}
              >
                {t("Ask AI to fill unresolved values")}
              </Button>
            ) : null}
            {editable ? (
              <Button variant="danger" className="right" onClick={() => setReject(true)}>
                {t("Reject document")}
              </Button>
            ) : null}
          </div>
          <div className="tiny">
            {t(
              "Nothing here changes stock, costs or supplier balances. A receiving draft is posted on the Receiving drafts tab by someone allowed to receive stock.",
            )}
          </div>
        </div>
      </FeatureGate>
      {picking ? (
        <ProductPick
          initial={picking.description ?? ""}
          onClose={() => setPicking(null)}
          onPick={(pid) => (void patchLine(picking, { product_id: pid }), setPicking(null))}
        />
      ) : null}
      {reject ? (
        <Confirm
          title={t("Reject document")}
          confirmLabel={t("Reject")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setReject(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.invoiceScan.reject(id, reason));
            if (r) {
              setReject(false);
              void reload();
            }
          }}
        >
          <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} />
        </Confirm>
      ) : null}
    </div>
  );
}

function FieldEditor({
  kind,
  field,
  label,
  onCommit,
}: {
  kind: string;
  field: DocField;
  label: string;
  onCommit: (v: string | number) => void;
}) {
  const initial =
    field.value === null || field.value === undefined
      ? ""
      : kind === "money" && typeof field.value === "number"
        ? formatAmount(field.value)
        : String(field.value);
  const [v, setV] = useState(initial);
  useEffect(() => setV(initial), [initial]);
  return (
    <input
      className="input"
      aria-label={label}
      type={kind === "date" ? "date" : "text"}
      value={v}
      style={{ width: kind === "money" ? 110 : 150 }}
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => {
        if (v === initial) return;
        if (kind === "money") {
          const m = parseMoney(v);
          if (m !== null) onCommit(m);
        } else onCommit(v);
      }}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
    />
  );
}

function SupplierField({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
}) {
  const { data } = useLoad(() => api.suppliers.list(), []);
  return (
    <Field label={t("Supplier")}>
      <select
        className="select"
        aria-label={t("Supplier")}
        disabled={disabled}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="">{t("Choose…")}</option>
        {(data ?? []).map((s) => (
          <option key={s.supplier_id} value={s.supplier_id}>
            {s.name}
          </option>
        ))}
      </select>
    </Field>
  );
}

/** Queue health for the document pipeline (Section: observability). */
export function DocMetricsStrip() {
  const { data } = useLoad(() => api.docs.metrics(), []);
  if (!data || !data.documents) return null;
  return (
    <div className="row small" style={{ flexWrap: "wrap", gap: 16 }} data-testid="doc-metrics">
      <span>{t("Documents: {0}", data.documents)}</span>
      <span>{t("Read successfully: {0}%", data.extraction_success_pct)}</span>
      <span>{t("Supplier matched automatically: {0}%", data.supplier_auto_match_pct)}</span>
      <span>{t("Products matched automatically: {0}%", data.product_auto_match_pct)}</span>
      <span>{t("Lines corrected by people: {0}%", data.correction_rate_pct)}</span>
      <span>{t("Duplicates caught: {0}", data.duplicates_detected)}</span>
    </div>
  );
}

// ---- Receiving drafts (posted by a person with receiving rights)

export function ReceivingDraftsPanel() {
  const { data, loading, error, reload } = useLoad(() => api.receivingDrafts.list(), []);
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div className="col gap-16">
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<ReceivingDraft>
        rows={data}
        loading={loading}
        rowKey={(r) => r.draft_id}
        onRowClick={(r) => setOpen(r.draft_id)}
        empty={<div className="empty">{t("No receiving drafts yet.")}</div>}
        columns={[
          { key: "n", label: t("Draft"), render: (r) => r.number },
          { key: "sup", label: t("Supplier"), render: (r) => r.supplier_name },
          { key: "po", label: t("Purchase order"), render: (r) => r.po_number ?? "—" },
          { key: "doc", label: t("Document"), render: (r) => r.scan_number ?? "—" },
          { key: "at", label: t("Created"), render: (r) => formatDateTime(r.created_at), sort: (r) => r.created_at },
          { key: "tot", label: t("Cost total"), num: true, render: (r) => formatMoney(r.total_minor) },
          {
            key: "st",
            label: t("Status"),
            render: (r) => (
              <Chip tone={r.status === "posted" ? "success" : r.status === "cancelled" ? "default" : "warning"}>
                {DRAFT_STATUS[r.status]?.() ?? r.status}
              </Chip>
            ),
          },
        ]}
      />
      {open ? <ReceivingDraftDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
    </div>
  );
}

const DRAFT_STATUS: Record<string, () => string> = {
  draft: () => t("Draft"),
  posted: () => t("Posted"),
  cancelled: () => t("Cancelled"),
};

function ReceivingDraftDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const { data, error, setData } = useLoad(() => api.receivingDrafts.get(id), [id]);
  const act = useAction();
  const [post, setPost] = useState(false);
  const [cancel, setCancel] = useState(false);
  const editable = data?.status === "draft" && has("purchasing.manage");
  return (
    <Drawer wide title={data ? `${t("Receiving draft")} ${data.number}` : t("Receiving draft")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          <dl className="kv">
            <dt>{t("Supplier")}</dt>
            <dd>{data.supplier_name}</dd>
            <dt>{t("Purchase order")}</dt>
            <dd>{data.po_id ? <Link to={`/admin/purchase-orders/${data.po_id}`}>{data.po_number}</Link> : "—"}</dd>
            <dt>{t("Document")}</dt>
            <dd>{data.scan_id ? <Link to={`/admin/invoice-scan/${data.scan_id}`}>{data.scan_number}</Link> : "—"}</dd>
            <dt>{t("Status")}</dt>
            <dd>{DRAFT_STATUS[data.status]?.() ?? data.status}</dd>
            {data.posted_at ? (
              <>
                <dt>{t("Posted")}</dt>
                <dd>
                  {formatDateTime(data.posted_at)} · {data.posted_by_name}
                </dd>
              </>
            ) : null}
          </dl>
          <div className="table-wrap card">
            <table className="table">
              <thead>
                <tr>
                  <th>{t("Product")}</th>
                  <th className="num">{t("Qty")}</th>
                  <th className="num">{t("Unit cost")}</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {(data.lines ?? []).map((l) => (
                  <tr key={l.line_no}>
                    <td>
                      {l.product_name}
                      <div className="tiny" dir="auto">
                        {l.description}
                      </div>
                    </td>
                    <td className="num">
                      <InlineNumber
                        disabled={!editable}
                        value={formatQty(l.qty_milli)}
                        onCommit={async (x) => {
                          const q = parseQty(x);
                          if (q === null) return;
                          const r = await act.run(() =>
                            api.receivingDrafts.updateLine(id, l.line_no, { qty_milli: q }),
                          );
                          if (r) setData(r);
                        }}
                      />
                    </td>
                    <td className="num">
                      <InlineNumber
                        disabled={!editable}
                        value={l.unit_cost_minor === null ? "" : formatAmount(l.unit_cost_minor)}
                        onCommit={async (x) => {
                          const c = parseMoney(x);
                          if (c === null) return;
                          const r = await act.run(() =>
                            api.receivingDrafts.updateLine(id, l.line_no, { unit_cost_minor: c }),
                          );
                          if (r) setData(r);
                        }}
                      />
                    </td>
                    <td>
                      {editable ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={async () => {
                            const r = await act.run(() =>
                              api.receivingDrafts.updateLine(id, l.line_no, { remove: true }),
                            );
                            if (r) setData(r);
                          }}
                        >
                          {t("Remove")}
                        </Button>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="small">
            {t("Cost total")}: <strong>{formatMoney(data.total_minor)}</strong>
          </div>
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {data.status === "draft" ? (
            <div className="row">
              {has("inventory.receive") ? (
                <Button variant="primary" data-testid="draft-post" onClick={() => setPost(true)}>
                  {t("Receive this stock")}
                </Button>
              ) : (
                <span className="tiny">{t("Someone allowed to receive stock posts this draft.")}</span>
              )}
              {editable ? (
                <Button variant="danger" className="right" onClick={() => setCancel(true)}>
                  {t("Cancel draft")}
                </Button>
              ) : null}
            </div>
          ) : null}
        </div>
      )}
      {post && data ? (
        <Confirm
          title={t("Receive this stock")}
          confirmLabel={t("Receive stock")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setPost(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.receivingDrafts.post(id, newOperationId()));
            if (r) {
              setData(r);
              setPost(false);
              onChanged();
              toast("success", t("Stock received"));
            }
          }}
        >
          {t(
            "{0} lines are added to stock at the costs shown, through normal receiving. This is recorded in the audit log.",
            (data.lines ?? []).length,
          )}
        </Confirm>
      ) : null}
      {cancel ? (
        <Confirm
          title={t("Cancel draft")}
          confirmLabel={t("Cancel draft")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setCancel(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.receivingDrafts.cancel(id));
            if (r) {
              setData(r);
              setCancel(false);
              onChanged();
            }
          }}
        >
          {t("The draft is cancelled. No stock has changed.")}
        </Confirm>
      ) : null}
    </Drawer>
  );
}

// ---- Supplier invoice records (review only: no payables ledger)

const SI_STATUS: Record<string, () => string> = {
  draft: () => t("Draft"),
  approved: () => t("Approved"),
  void: () => t("Void"),
};

export function SupplierInvoicesPanel() {
  const { data, loading, error, reload } = useLoad(() => api.supplierInvoices.list(), []);
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div className="col gap-16">
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<SupplierInvoice>
        rows={data}
        loading={loading}
        rowKey={(r) => r.invoice_id}
        onRowClick={(r) => setOpen(r.invoice_id)}
        empty={<div className="empty">{t("No supplier invoice records yet.")}</div>}
        columns={[
          { key: "n", label: t("Record"), render: (r) => r.number },
          { key: "sup", label: t("Supplier"), render: (r) => r.supplier_name },
          { key: "inv", label: t("Invoice"), render: (r) => r.invoice_number ?? "—" },
          { key: "d", label: t("Invoice date"), render: (r) => r.invoice_date ?? "—" },
          {
            key: "tot",
            label: t("Invoice total"),
            num: true,
            render: (r) => (r.total_minor === null ? "—" : formatMoney(r.total_minor)),
          },
          {
            key: "st",
            label: t("Status"),
            render: (r) => (
              <Chip tone={r.status === "approved" ? "success" : r.status === "void" ? "default" : "warning"}>
                {SI_STATUS[r.status]?.() ?? r.status}
              </Chip>
            ),
          },
        ]}
      />
      {open ? <SupplierInvoiceDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
    </div>
  );
}

function SupplierInvoiceDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const { data, error, setData } = useLoad(() => api.supplierInvoices.get(id), [id]);
  const act = useAction();
  const [voiding, setVoiding] = useState(false);
  const set = async (st: "approved" | "void") => {
    const r = await act.run(() => api.supplierInvoices.setStatus(id, st));
    if (r) {
      setData(r);
      onChanged();
    }
  };
  return (
    <Drawer wide title={data ? `${t("Supplier invoice")} ${data.number}` : t("Supplier invoice")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          {data.posting_note ? <Banner tone="info">{tb(data.posting_note)}</Banner> : null}
          <dl className="kv">
            <dt>{t("Supplier")}</dt>
            <dd>{data.supplier_name}</dd>
            <dt>{t("Invoice number")}</dt>
            <dd>{data.invoice_number ?? "—"}</dd>
            <dt>{t("Invoice date")}</dt>
            <dd>{data.invoice_date ?? "—"}</dd>
            <dt>{t("Due date")}</dt>
            <dd>{data.due_date ?? "—"}</dd>
            <dt>{t("Subtotal")}</dt>
            <dd>{formatMoney(data.subtotal_minor ?? null)}</dd>
            <dt>{t("VAT")}</dt>
            <dd>{formatMoney(data.vat_minor ?? null)}</dd>
            <dt>{t("Invoice total")}</dt>
            <dd>{formatMoney(data.total_minor)}</dd>
            <dt>{t("Document")}</dt>
            <dd>{data.scan_id ? <Link to={`/admin/invoice-scan/${data.scan_id}`}>{data.scan_number}</Link> : "—"}</dd>
          </dl>
          <div className="table-wrap card">
            <table className="table">
              <thead>
                <tr>
                  <th>{t("Line")}</th>
                  <th className="num">{t("Qty")}</th>
                  <th className="num">{t("Unit cost")}</th>
                  <th className="num">{t("Line total")}</th>
                </tr>
              </thead>
              <tbody>
                {(data.lines ?? []).map((l) => (
                  <tr key={l.line_no}>
                    <td dir="auto">
                      {l.product_name ?? l.description}
                      {l.product_name ? <div className="tiny">{l.description}</div> : null}
                    </td>
                    <td className="num">{l.qty_milli === null ? "—" : formatQty(l.qty_milli)}</td>
                    <td className="num">{formatMoney(l.unit_cost_minor)}</td>
                    <td className="num">{formatMoney(l.line_total_minor)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {data.status === "draft" && has("purchasing.manage") ? (
            <div className="row">
              <Button variant="primary" onClick={() => void set("approved")} loading={act.busy}>
                {t("Mark reviewed and approved")}
              </Button>
              <Button variant="danger-outline" className="right" onClick={() => setVoiding(true)}>
                {t("Void record")}
              </Button>
            </div>
          ) : null}
        </div>
      )}
      {voiding && data ? (
        <Confirm
          title={t("Void {0}?", data.number)}
          confirmLabel={t("Void record")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setVoiding(false)}
          onConfirm={async () => {
            await set("void");
            setVoiding(false);
          }}
        >
          {t("The record is kept for the history but can no longer be approved or posted. This cannot be undone.")}
        </Confirm>
      ) : null}
    </Drawer>
  );
}
