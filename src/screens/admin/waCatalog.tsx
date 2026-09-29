// WhatsApp → Catalogue: publish the POS catalogue to the linked WhatsApp
// Business account, through the same WhatsApp link that sends receipts.
// The POS stays the source of truth; nothing is published until an owner
// presses "Sync catalogue" for the linked number.
import { useEffect, useState } from "react";
import { RefreshCw, Store } from "lucide-react";
import { api } from "../../api";
import type {
  WaCatalogCapability,
  WaCatalogItemStatus,
  WaCatalogOverview,
  WaCatalogProductState,
  WaCatalogRun,
  WaCatalogStatus,
} from "../../api/types";
import { Banner, Button, Checkbox, Chip, Skeleton } from "../../components/ui";
import { useFeature } from "../../components/FeatureGate";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Confirm } from "./common";
import { formatDateTime } from "../../lib/time";
import { t, tb } from "../../i18n";

export function capabilityText(c: WaCatalogCapability): string {
  switch (c) {
    case "supported":
      return t("WhatsApp Business account with a catalogue");
    case "personal":
      return t("Personal WhatsApp account: catalogues need WhatsApp Business");
    case "business_no_catalog":
      return t("WhatsApp Business account, but its catalogue could not be read");
    case "unavailable":
      return t("Could not check the catalogue right now");
    case "unsupported":
      return t("This WhatsApp client cannot manage catalogues");
    case "terminal":
      return t("Catalogue publishing runs on the hub computer");
    case "checking":
      return t("Checking the linked account…");
    default:
      return t("WhatsApp is not connected");
  }
}

export function itemStatusText(s: WaCatalogItemStatus | null | undefined): string {
  switch (s) {
    case "synced":
      return t("On WhatsApp");
    case "queued":
    case "syncing":
      return t("Waiting to publish");
    case "hidden":
      return t("Hidden on WhatsApp");
    case "removed":
      return t("Removed from WhatsApp");
    case "failed":
      return t("Could not publish");
    case "remote_missing":
      return t("Deleted on WhatsApp");
    default:
      return t("Not on WhatsApp");
  }
}

/** "47 / 182 products processed" and the bar width (0–100). */
export function runProgress(run: WaCatalogRun): { label: string; pct: number; done: boolean } {
  const total = Math.max(run.total, 0);
  const processed = Math.min(run.processed, total);
  const pct = total === 0 ? 100 : Math.floor((processed * 100) / total);
  return {
    label: t("{0} / {1} products processed", processed, total),
    pct,
    done: processed >= total && run.verify !== "pending",
  };
}

/** Rough duration of a first sync: about one product a second, plus pictures. */
export function estimateMinutes(products: number): number {
  return Math.max(1, Math.ceil((products * 1.5) / 60));
}

function RunCounts({ run }: { run: WaCatalogRun }) {
  return (
    <div className="row wrap gap-8" data-testid="wa-catalog-run-counts">
      <Chip tone="success">{t("Published: {0}", run.synced)}</Chip>
      <Chip>{t("Already up to date: {0}", run.unchanged)}</Chip>
      {run.hidden ? <Chip>{t("Hidden: {0}", run.hidden)}</Chip> : null}
      {run.removed ? <Chip>{t("Removed: {0}", run.removed)}</Chip> : null}
      <Chip tone={run.failed ? "danger" : "default"}>{t("Could not publish: {0}", run.failed)}</Chip>
    </div>
  );
}

function RunPanel({ run, running, lastError }: { run: WaCatalogRun; running: boolean; lastError: string | null }) {
  const p = runProgress(run);
  return (
    <div className="col gap-8" data-testid={running ? "wa-catalog-run" : "wa-catalog-last-run"}>
      <div className="row">
        <strong className="grow">{running ? t("Sync in progress") : t("Last full sync")}</strong>
        <span className="tiny muted">
          {running ? formatDateTime(run.started_at) : run.finished_at ? formatDateTime(run.finished_at) : ""}
        </span>
      </div>
      {running ? (
        <div
          className="wa-progress"
          role="progressbar"
          aria-label={t("Catalogue sync progress")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={p.pct}
        >
          <span style={{ width: `${p.pct}%` }} />
        </div>
      ) : null}
      <div className="num">{p.label}</div>
      <RunCounts run={run} />
      {run.verify === "pending" && running ? (
        <div className="tiny muted">{t("Checking the WhatsApp catalogue for products deleted there…")}</div>
      ) : run.verify === "skipped" ? (
        <div className="tiny muted">
          {t("The WhatsApp catalogue could not be read completely, so products deleted there were not looked for.")}
        </div>
      ) : null}
      {running ? (
        <div className="tiny muted">
          {t(
            "Runs in the background, about one product a second. Receipts and WhatsApp messages keep going; you can leave this page.",
          )}
        </div>
      ) : null}
      {running && lastError ? (
        <div className="tiny" data-testid="wa-catalog-last-error">
          {t("Last problem")}: {tb(lastError)}
        </div>
      ) : null}
    </div>
  );
}

function FirstSyncExplainer({ cat }: { cat: WaCatalogOverview }) {
  const left = cat.not_publishable;
  return (
    <Banner tone="info" title={t("Before the first sync")}>
      <ul className="col gap-8" data-testid="wa-catalog-explainer">
        <li>{t("Nothing is sent to WhatsApp until you press Sync.")}</li>
        <li>
          {t(
            "{0} active products with a price will be published, with their AMWAPOS name, price, description and product code.",
            cat.publishable,
          )}
        </li>
        <li>
          {t(
            "Left out: {0} archived, {1} without a price, {2} with a price WhatsApp cannot show.",
            left.archived ?? 0,
            left.no_price ?? 0,
            left.price_not_supported ?? 0,
          )}
        </li>
        <li>
          {t(
            "Pictures come only from AMWAPOS product pictures. No picture search is started; products without a picture are published without one.",
          )}
        </li>
        <li>
          {t(
            "Products you created yourself in WhatsApp Business are never changed or deleted. A WhatsApp product with the same product code is linked instead of duplicated.",
          )}
        </li>
        <li>{t("Categories are not created as WhatsApp collections: this WhatsApp link cannot write them.")}</li>
        <li>
          {t(
            "It takes about {0} min in the background. Receipts and messages keep going.",
            estimateMinutes(cat.publishable),
          )}
        </li>
      </ul>
    </Banner>
  );
}

export function WaCatalog() {
  const toast = useToast();
  const { has } = useSession();
  const canManage = has("whatsapp.manage") && has("products.manage");
  const [st, setSt] = useState<WaCatalogStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | "sync" | "retry" | "auto" | "check">(null);
  const [confirm, setConfirm] = useState(false);
  const load = () =>
    api.whatsapp
      .catalogStatus()
      .then((s) => (setSt(s), setError(null)))
      .catch((e: Error) => setError(e.message));
  const running = !!st?.catalog.run;
  useEffect(() => {
    void load();
    // Faster while a sync runs, so the progress moves.
    const id = setInterval(() => void load(), running ? 1500 : 4000);
    return () => clearInterval(id);
  }, [running]);
  const run = async (kind: "sync" | "retry" | "auto" | "check", fn: () => Promise<unknown>, ok?: string) => {
    setBusy(kind);
    try {
      await fn();
      if (ok) toast("success", ok);
      await load();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(null);
    }
  };
  if (!st) return error ? <Banner tone="danger">{tb(error)}</Banner> : <Skeleton />;
  const cap = st.capability;
  const cat = st.catalog;
  const supported = cap.capability === "supported";
  const c = cat.counts;
  const retrying = cat.retrying ?? 0;
  const pending = Math.max((c.queued ?? 0) + (c.syncing ?? 0) - retrying, 0);
  const failed = (c.failed ?? 0) + (c.remote_missing ?? 0);
  return (
    <div className="col gap-16" data-testid="wa-catalog" data-capability={cap.capability}>
      <div className="card card-pad col gap-12">
        <div className="row">
          <Store size={20} aria-hidden />
          <h3 className="grow">{t("WhatsApp catalogue")}</h3>
          <Chip tone={supported ? "success" : "default"}>{capabilityText(cap.capability)}</Chip>
        </div>
        <div className="tiny">
          {t(
            "Publishes this shop's products to the WhatsApp Business catalogue of the linked number, through the same WhatsApp link that sends receipts. AMWAPOS stays the source of truth: prices, names, descriptions and pictures flow from AMWAPOS to WhatsApp, never back.",
          )}
        </div>
        {cap.capability === "terminal" ? (
          <div className="tiny" data-testid="wa-catalog-terminal">
            {t(
              "The WhatsApp catalogue is managed on the hub computer, where WhatsApp is linked. Open this page there.",
            )}
          </div>
        ) : null}
        {cap.detail ? <div className="tiny muted">{tb(cap.detail)}</div> : null}
        <dl className="kv" data-testid="wa-catalog-capability">
          <dt>{t("Linked number")}</dt>
          <dd>{cap.account ? `+${cap.account}` : "—"}</dd>
          <dt>{t("Products")}</dt>
          <dd>{supported ? t("Supported") : t("Not available")}</dd>
          <dt>{t("Collections (categories)")}</dt>
          <dd>
            {cap.collections
              ? t("Supported")
              : t("Not supported by this WhatsApp link: products are published without collections.")}
          </dd>
          {cap.checked_at ? (
            <>
              <dt>{t("Checked")}</dt>
              <dd>{formatDateTime(cap.checked_at)}</dd>
            </>
          ) : null}
        </dl>
        {canManage && cap.capability !== "terminal" ? (
          <div className="row wrap">
            <Button
              icon={<RefreshCw size={16} />}
              loading={busy === "check"}
              disabled={!!busy}
              onClick={() => run("check", () => api.whatsapp.catalogRecheck())}
            >
              {t("Check again")}
            </Button>
          </div>
        ) : null}
      </div>

      {supported ? (
        <div className="card card-pad col gap-12">
          {!cat.published && !cat.run ? <FirstSyncExplainer cat={cat} /> : null}
          {cat.run ? (
            <RunPanel run={cat.run} running lastError={cap.last_error} />
          ) : cat.last_run ? (
            <RunPanel run={cat.last_run} running={false} lastError={null} />
          ) : null}
          <dl className="kv" data-testid="wa-catalog-counts">
            <dt>{t("Products that can be published")}</dt>
            <dd className="num">{cat.publishable}</dd>
            <dt>{t("On WhatsApp")}</dt>
            <dd className="num">{c.synced ?? 0}</dd>
            {cat.out_of_date ? (
              <>
                <dt>{t("Changed in AMWAPOS since the last sync")}</dt>
                <dd className="num">{cat.out_of_date}</dd>
              </>
            ) : null}
            <dt>{t("Waiting to publish")}</dt>
            <dd className="num">{pending}</dd>
            {retrying ? (
              <>
                <dt>{t("Retrying automatically")}</dt>
                <dd className="num">{retrying}</dd>
              </>
            ) : null}
            <dt>{t("Hidden on WhatsApp")}</dt>
            <dd className="num">{c.hidden ?? 0}</dd>
            <dt>{t("Could not publish")}</dt>
            <dd className="num">{c.failed ?? 0}</dd>
            {c.remote_missing ? (
              <>
                <dt>{t("Deleted on WhatsApp")}</dt>
                <dd className="num">{c.remote_missing}</dd>
              </>
            ) : null}
            <dt>{t("Left out (no price)")}</dt>
            <dd className="num">{cat.not_publishable.no_price ?? 0}</dd>
            {cat.not_publishable.price_not_supported ? (
              <>
                <dt>{t("Left out (price WhatsApp cannot show)")}</dt>
                <dd className="num">{cat.not_publishable.price_not_supported}</dd>
              </>
            ) : null}
            <dt>{t("Last published")}</dt>
            <dd>{cat.last_synced_at ? formatDateTime(cat.last_synced_at) : "—"}</dd>
          </dl>
          {cat.out_of_date && !cat.auto_sync ? (
            <div className="tiny muted">{t("Automatic sync is off: changes reach WhatsApp at the next Sync.")}</div>
          ) : null}
          {cat.failures.length ? (
            <div className="col gap-8" data-testid="wa-catalog-failures">
              <h4>{t("Needs attention")}</h4>
              {cat.failures.map((f) => (
                <div key={f.product_id} className="row">
                  <span className="grow ellipsis" title={f.detail ?? undefined}>
                    <strong>{f.name ?? f.product_id}</strong> · {itemStatusText(f.status as WaCatalogItemStatus)}
                    {f.error ? ` · ${tb(f.error)}` : ""}
                  </span>
                  {canManage ? (
                    <Button size="sm" onClick={() => run("retry", () => api.whatsapp.catalogRetry(f.product_id))}>
                      {t("Retry")}
                    </Button>
                  ) : null}
                </div>
              ))}
            </div>
          ) : null}
          {canManage ? (
            <>
              <Checkbox
                label={t("Keep the WhatsApp catalogue synchronised automatically")}
                checked={cat.auto_sync}
                disabled={!!busy}
                onChange={(v) => run("auto", () => api.whatsapp.catalogConfigure(v), t("Settings saved"))}
              />
              <div className="tiny muted">
                {t(
                  "On: product changes are published within about a minute. Off: nothing changes on WhatsApp until you press Sync. Products deleted in WhatsApp are only published again by Sync or Retry.",
                )}
              </div>
              <div className="row wrap">
                <Button
                  variant="primary"
                  loading={busy === "sync"}
                  disabled={!!busy || running}
                  onClick={() => setConfirm(true)}
                >
                  {running
                    ? t("Sync in progress")
                    : cat.published
                      ? t("Sync catalogue now")
                      : t("Sync catalogue to WhatsApp")}
                </Button>
                {failed > 0 ? (
                  <Button
                    loading={busy === "retry"}
                    disabled={!!busy}
                    onClick={() => run("retry", () => api.whatsapp.catalogRetry())}
                  >
                    {t("Retry failed")}
                  </Button>
                ) : null}
              </div>
            </>
          ) : null}
        </div>
      ) : null}
      {error ? <Banner tone="danger">{tb(error)}</Banner> : null}
      {confirm ? (
        <Confirm
          title={cat.published ? t("Sync catalogue now") : t("Sync catalogue to WhatsApp")}
          confirmLabel={t("Start")}
          onCancel={() => setConfirm(false)}
          onConfirm={() => {
            setConfirm(false);
            void run("sync", () => api.whatsapp.catalogSync(), t("Catalogue sync started"));
          }}
        >
          {t(
            "{0} products will be published or updated in the WhatsApp catalogue of +{1}. Products you created directly in WhatsApp are not changed.",
            cat.publishable,
            cap.account ?? "",
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

/** Product editor: one line about the product's WhatsApp catalogue copy. */
export function WaCatalogProductLine({ productId }: { productId: string }) {
  const on = useFeature("whatsapp.enabled");
  const { has } = useSession();
  const [st, setSt] = useState<WaCatalogProductState | null>(null);
  useEffect(() => {
    if (!on || !has("whatsapp.manage")) return;
    api.whatsapp
      .catalogProduct(productId)
      .then(setSt)
      .catch(() => undefined);
  }, [on, productId, has]);
  if (!st || !st.published) return null;
  return (
    <div className="tiny muted" data-testid="wa-catalog-product">
      {t("WhatsApp catalogue")}: {itemStatusText(st.status)}
      {st.out_of_date ? ` · ${t("changed since the last sync")}` : ""}
      {st.picture_refused ? ` · ${t("WhatsApp refused the picture; published without it")}` : ""}
      {st.last_error ? ` · ${tb(st.last_error)}` : ""}
    </div>
  );
}
