import { useEffect, useMemo, useState } from "react";
import { Check, Network, Store, Server, Search } from "lucide-react";
import { api } from "../../api";
import { explain } from "../../lib/errors";
import { formatPercent, parsePercent } from "../../lib/money";
import { Banner, Button, Checkbox, Field, TextInput } from "../../components/ui";
import { Logo } from "../../components/Logo";

type Path = "new" | "join";

interface Draft {
  path: Path;
  mode: "standalone" | "hub";
  business_name: string;
  business_name_ar: string;
  cr_number: string;
  vat_number: string;
  phone: string;
  address: string;
  currency: string;
  timezone: string;
  branch_name: string;
  branch_code: string;
  vat_rate: string;
  prices_include_vat: boolean;
  owner_name: string;
  device_name: string;
  device_code: string;
  header: string;
  footer: string;
  paper_width_mm: number;
  printer_mode: string;
  printer_target: string;
  backup_directory: string;
  hub_url: string;
  pair_code: string;
}

const DRAFT_KEY = "amwapos.setup.draft";

const initial: Draft = {
  path: "new",
  mode: "standalone",
  business_name: "",
  business_name_ar: "",
  cr_number: "",
  vat_number: "",
  phone: "",
  address: "",
  currency: "BHD",
  timezone: "Asia/Bahrain",
  branch_name: "Main Branch",
  branch_code: "MAIN",
  vat_rate: "10",
  prices_include_vat: true,
  owner_name: "",
  device_name: "Till 1",
  device_code: "T01",
  header: "",
  footer: "Thank you for shopping with us",
  paper_width_mm: 80,
  printer_mode: "none",
  printer_target: "",
  backup_directory: "",
  hub_url: "",
  pair_code: "",
};

const NEW_STEPS = ["Welcome", "Business", "Branch", "Tax", "Owner", "Terminal", "Receipt", "Printer", "Backup", "Review"] as const;
const JOIN_STEPS = ["Welcome", "Hub", "Terminal", "Review"] as const;

export function SetupWizard({ onDone }: { onDone: () => Promise<void> }) {
  const [d, setD] = useState<Draft>(() => {
    try {
      const raw = localStorage.getItem(DRAFT_KEY);
      return raw ? { ...initial, ...JSON.parse(raw) } : initial;
    } catch {
      return initial;
    }
  });
  const [step, setStep] = useState(0);
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [found, setFound] = useState<{ url: string; hub_name: string; business_name: string }[] | null>(null);
  const [probe, setProbe] = useState<Record<string, unknown> | null>(null);
  const steps: readonly string[] = d.path === "join" ? JOIN_STEPS : NEW_STEPS;
  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setD((x) => ({ ...x, [k]: v }));

  // Resumable: the draft (never the PIN) survives interruptions.
  useEffect(() => {
    try {
      localStorage.setItem(DRAFT_KEY, JSON.stringify(d));
    } catch {
      /* storage unavailable */
    }
  }, [d]);

  const vatBp = parsePercent(d.vat_rate);
  const stepError = useMemo((): string | null => {
    const name = steps[step];
    switch (name) {
      case "Business":
        return d.business_name.trim() ? null : "Enter the business name.";
      case "Branch":
        if (!d.branch_name.trim()) return "Enter the branch name.";
        return /^[A-Za-z0-9]{1,8}$/.test(d.branch_code) ? null : "Branch code must be 1–8 letters or digits.";
      case "Tax":
        return vatBp !== null && vatBp >= 0 && vatBp <= 10000 ? null : "Enter a VAT rate between 0 and 100.";
      case "Owner":
        if (!d.owner_name.trim()) return "Enter the owner's name.";
        if (!/^\d{4,8}$/.test(pin)) return "The PIN must be 4–8 digits.";
        if (pin !== pin2) return "The two PINs do not match.";
        return null;
      case "Terminal":
        if (!d.device_name.trim()) return "Enter a terminal name.";
        return /^[A-Za-z0-9]{1,8}$/.test(d.device_code) ? null : "Terminal code must be 1–8 letters or digits.";
      case "Hub":
        if (!d.hub_url.trim()) return "Enter or discover the hub address.";
        return /^\d{8}$/.test(d.pair_code) ? null : "Enter the 8-digit pairing code shown on the hub.";
      default:
        return null;
    }
  }, [steps, step, d, pin, pin2, vatBp]);

  const finish = async () => {
    setBusy(true);
    setError(null);
    try {
      if (d.path === "join") {
        await api.sync.join({ hub_url: d.hub_url, code: d.pair_code, device_name: d.device_name, device_code: d.device_code.toUpperCase() });
      } else {
        await api.setup.initialize({
          business_name: d.business_name,
          business_name_ar: d.business_name_ar || null,
          cr_number: d.cr_number || null,
          vat_number: d.vat_number || null,
          phone: d.phone || null,
          address: d.address || null,
          currency: d.currency,
          currency_digits: d.currency === "BHD" || d.currency === "KWD" || d.currency === "OMR" ? 3 : 2,
          timezone: d.timezone,
          branch_name: d.branch_name,
          branch_code: d.branch_code.toUpperCase(),
          vat_rate_bp: vatBp,
          prices_include_vat: d.prices_include_vat,
          owner_name: d.owner_name,
          owner_pin: pin,
          device_name: d.device_name,
          device_code: d.device_code.toUpperCase(),
          mode: d.mode,
          receipt: {
            header_lines: d.header ? d.header.split("\n") : [],
            footer_lines: d.footer ? d.footer.split("\n") : [],
            show_vat_number: true,
            show_cr_number: true,
            show_cashier: true,
            show_barcode: false,
            paper_width_mm: d.paper_width_mm,
            title: "TAX INVOICE",
          },
          printer: { mode: d.printer_mode, target: d.printer_target, paper_width_mm: d.paper_width_mm, cut: true, drawer_pulse: true, code_page: "cp437" },
          backup_directory: d.backup_directory || null,
        });
      }
      localStorage.removeItem(DRAFT_KEY);
      await onDone();
    } catch (e) {
      const ex = explain(e);
      setError(`${ex.message} ${ex.action}`);
    } finally {
      setBusy(false);
    }
  };

  const next = () => {
    if (stepError) {
      setError(stepError);
      return;
    }
    setError(null);
    if (step === steps.length - 1) void finish();
    else setStep(step + 1);
  };

  const discover = async () => {
    setFound(null);
    try {
      setFound(await api.sync.discover());
    } catch (e) {
      setError(explain(e).message);
      setFound([]);
    }
  };
  const test = async () => {
    setProbe(null);
    setError(null);
    try {
      const r = await api.sync.probe(d.hub_url);
      set("hub_url", r.url);
      setProbe(r.info);
    } catch (e) {
      setError(explain(e).message);
    }
  };

  const name = steps[step];
  return (
    <div className="wizard">
      <aside className="wizard-steps" aria-label="Setup steps">
        <div className="row" style={{ marginBottom: 20 }}>
          <Logo size={30} />
          <strong>AMWAPOS Setup</strong>
        </div>
        {steps.map((s, i) => (
          <div key={s} className={`wizard-step ${i === step ? "active" : i < step ? "done" : ""}`} aria-current={i === step ? "step" : undefined}>
            <span className="n">{i < step ? <Check size={14} /> : i + 1}</span>
            {s}
          </div>
        ))}
      </aside>
      <main className="wizard-main">
        <div className="wizard-body">
          <div className="inner stack-24">
            {name === "Welcome" ? (
              <>
                <div>
                  <h1>Welcome to AMWAPOS</h1>
                  <p className="muted">Set up this computer. Everything runs locally; the Internet is not required to sell.</p>
                </div>
                <div className="choice-cards">
                  <button className={`choice-card ${d.path === "new" && d.mode === "standalone" ? "active" : ""}`} onClick={() => setD({ ...d, path: "new", mode: "standalone" })}>
                    <Store size={22} color="var(--brand)" />
                    <h3>New store (single computer)</h3>
                    <div className="small muted">Create the business on this computer.</div>
                  </button>
                  <button className={`choice-card ${d.path === "new" && d.mode === "hub" ? "active" : ""}`} onClick={() => setD({ ...d, path: "new", mode: "hub" })}>
                    <Server size={22} color="var(--brand)" />
                    <h3>New store as hub</h3>
                    <div className="small muted">This computer holds the master data; other tills connect over the store network.</div>
                  </button>
                  <button className={`choice-card ${d.path === "join" ? "active" : ""}`} onClick={() => setD({ ...d, path: "join", device_name: "Till 2", device_code: "T02" })}>
                    <Network size={22} color="var(--brand)" />
                    <h3>Join an existing hub</h3>
                    <div className="small muted">Pair this till with the store's hub using a code.</div>
                  </button>
                </div>
              </>
            ) : null}
            {name === "Business" ? (
              <>
                <h2>Business identity</h2>
                <div className="form-grid">
                  <TextInput label="Business name" required value={d.business_name} onChange={(e) => set("business_name", e.target.value)} autoFocus fieldClass="span-2" />
                  <TextInput label="Arabic name" value={d.business_name_ar} dir="rtl" onChange={(e) => set("business_name_ar", e.target.value)} />
                  <TextInput label="Phone" value={d.phone} onChange={(e) => set("phone", e.target.value)} />
                  <TextInput label="CR number" value={d.cr_number} onChange={(e) => set("cr_number", e.target.value)} />
                  <TextInput label="VAT number" value={d.vat_number} onChange={(e) => set("vat_number", e.target.value)} />
                  <TextInput label="Address" value={d.address} onChange={(e) => set("address", e.target.value)} fieldClass="span-2" />
                </div>
              </>
            ) : null}
            {name === "Branch" ? (
              <>
                <h2>Branch</h2>
                <div className="form-grid">
                  <TextInput label="Branch name" required value={d.branch_name} onChange={(e) => set("branch_name", e.target.value)} autoFocus />
                  <TextInput label="Branch code" required value={d.branch_code} maxLength={8} onChange={(e) => set("branch_code", e.target.value.toUpperCase())} hint="Short code used in backups and reports." />
                  <Field label="Currency" hint="Cannot be changed after the first sale.">
                    <select className="select" value={d.currency} onChange={(e) => set("currency", e.target.value)}>
                      <option value="BHD">BHD — Bahraini Dinar (3 decimals)</option>
                      <option value="SAR">SAR — Saudi Riyal</option>
                      <option value="AED">AED — UAE Dirham</option>
                      <option value="KWD">KWD — Kuwaiti Dinar (3 decimals)</option>
                      <option value="OMR">OMR — Omani Rial (3 decimals)</option>
                      <option value="QAR">QAR — Qatari Riyal</option>
                    </select>
                  </Field>
                  <Field label="Timezone">
                    <select className="select" value={d.timezone} onChange={(e) => set("timezone", e.target.value)}>
                      {["Asia/Bahrain", "Asia/Riyadh", "Asia/Dubai", "Asia/Kuwait", "Asia/Muscat", "Asia/Qatar"].map((z) => (
                        <option key={z}>{z}</option>
                      ))}
                    </select>
                  </Field>
                </div>
              </>
            ) : null}
            {name === "Tax" ? (
              <>
                <h2>Tax</h2>
                <p className="muted">
                  Tax rules are versioned and can be changed later. Verify the current VAT rate with your tax adviser.
                </p>
                <div className="form-grid">
                  <TextInput label="Standard VAT rate (%)" required value={d.vat_rate} onChange={(e) => set("vat_rate", e.target.value)} inputMode="decimal" autoFocus hint={vatBp !== null ? `Stored as ${formatPercent(vatBp)}` : undefined} />
                  <Field label="Selling prices">
                    <Checkbox label="Prices include VAT" checked={d.prices_include_vat} onChange={(v) => set("prices_include_vat", v)} />
                  </Field>
                </div>
              </>
            ) : null}
            {name === "Owner" ? (
              <>
                <h2>Owner account</h2>
                <div className="form-grid">
                  <TextInput label="Owner name" required value={d.owner_name} onChange={(e) => set("owner_name", e.target.value)} autoFocus fieldClass="span-2" />
                  <TextInput label="PIN" required type="password" inputMode="numeric" autoComplete="new-password" value={pin} onChange={(e) => setPin(e.target.value.replace(/\D/g, "").slice(0, 8))} hint="4–8 digits. Avoid 1234 or repeated digits." />
                  <TextInput label="Confirm PIN" required type="password" inputMode="numeric" autoComplete="new-password" value={pin2} onChange={(e) => setPin2(e.target.value.replace(/\D/g, "").slice(0, 8))} />
                </div>
              </>
            ) : null}
            {name === "Terminal" ? (
              <>
                <h2>This terminal</h2>
                <div className="form-grid">
                  <TextInput label="Terminal name" required value={d.device_name} onChange={(e) => set("device_name", e.target.value)} autoFocus />
                  <TextInput label="Terminal code" required value={d.device_code} maxLength={8} onChange={(e) => set("device_code", e.target.value.toUpperCase())} hint="Prefix of receipt numbers, e.g. T01-0000001. Must be unique in the store." />
                </div>
              </>
            ) : null}
            {name === "Receipt" ? (
              <>
                <h2>Receipt</h2>
                <div className="form-grid">
                  <Field label="Header lines" className="span-2">
                    <textarea className="textarea" value={d.header} onChange={(e) => set("header", e.target.value)} placeholder="Optional, one per line" />
                  </Field>
                  <Field label="Footer lines" className="span-2">
                    <textarea className="textarea" value={d.footer} onChange={(e) => set("footer", e.target.value)} />
                  </Field>
                  <Field label="Paper width">
                    <select className="select" value={d.paper_width_mm} onChange={(e) => set("paper_width_mm", Number(e.target.value))}>
                      <option value={80}>80 mm</option>
                      <option value={58}>58 mm</option>
                    </select>
                  </Field>
                </div>
              </>
            ) : null}
            {name === "Printer" ? (
              <>
                <h2>Receipt printer</h2>
                <p className="muted">You can configure or test the printer later in Settings → Printers. A printer problem never cancels a sale.</p>
                <div className="form-grid">
                  <Field label="Connection">
                    <select className="select" value={d.printer_mode} onChange={(e) => set("printer_mode", e.target.value)}>
                      <option value="none">Configure later</option>
                      <option value="windows">Windows printer (spooler)</option>
                      <option value="network">Network printer (ESC/POS, port 9100)</option>
                    </select>
                  </Field>
                  {d.printer_mode !== "none" ? (
                    <TextInput label={d.printer_mode === "network" ? "Printer IP address" : "Windows printer name"} value={d.printer_target} onChange={(e) => set("printer_target", e.target.value)} placeholder={d.printer_mode === "network" ? "192.168.1.50:9100" : "EPSON TM-T20III Receipt"} />
                  ) : null}
                </div>
              </>
            ) : null}
            {name === "Backup" ? (
              <>
                <h2>Backups</h2>
                <p className="muted">Automatic daily backups are verified after creation. Choose a folder on a different disk or a USB drive if possible.</p>
                <TextInput label="Backup folder" value={d.backup_directory} onChange={(e) => set("backup_directory", e.target.value)} placeholder="Default: data folder\backups" />
              </>
            ) : null}
            {name === "Hub" ? (
              <>
                <h2>Connect to the hub</h2>
                <p className="muted">On the hub computer open Admin → Sync / Hub → Pair terminal to get a pairing code.</p>
                <div className="row" style={{ alignItems: "flex-end" }}>
                  <TextInput label="Hub address" value={d.hub_url} onChange={(e) => set("hub_url", e.target.value)} placeholder="192.168.1.10:47800" fieldClass="grow" autoFocus />
                  <Button icon={<Search size={16} />} onClick={discover}>
                    Find hub
                  </Button>
                  <Button onClick={test}>Test</Button>
                </div>
                {found ? (
                  found.length ? (
                    <div className="col">
                      {found.map((f) => (
                        <button key={f.url} className="choice-card" onClick={() => set("hub_url", f.url)}>
                          <strong>{f.hub_name}</strong> · {f.business_name} <span className="muted small">{f.url}</span>
                        </button>
                      ))}
                    </div>
                  ) : (
                    <Banner tone="warning">No hub answered on this network. Enter the address manually.</Banner>
                  )
                ) : null}
                {probe ? (
                  <Banner tone="success" title={`Found ${String(probe.business_name)}`}>
                    Hub {String(probe.hub_name)} · AMWAPOS {String(probe.app_version)}
                  </Banner>
                ) : null}
                <TextInput label="Pairing code" value={d.pair_code} maxLength={8} inputMode="numeric" onChange={(e) => set("pair_code", e.target.value.replace(/\D/g, ""))} />
              </>
            ) : null}
            {name === "Review" ? (
              <>
                <h2>Review</h2>
                {d.path === "join" ? (
                  <dl className="kv">
                    <dt>Hub</dt>
                    <dd>{d.hub_url}</dd>
                    <dt>Terminal</dt>
                    <dd>
                      {d.device_name} ({d.device_code})
                    </dd>
                  </dl>
                ) : (
                  <dl className="kv">
                    <dt>Business</dt>
                    <dd>{d.business_name}</dd>
                    <dt>Branch</dt>
                    <dd>
                      {d.branch_name} ({d.branch_code})
                    </dd>
                    <dt>Currency</dt>
                    <dd>{d.currency}</dd>
                    <dt>VAT</dt>
                    <dd>
                      {formatPercent(vatBp)} {d.prices_include_vat ? "included in prices" : "added to prices"}
                    </dd>
                    <dt>Owner</dt>
                    <dd>{d.owner_name}</dd>
                    <dt>Terminal</dt>
                    <dd>
                      {d.device_name} ({d.device_code}) · {d.mode === "hub" ? "Hub" : "Standalone"}
                    </dd>
                    <dt>Printer</dt>
                    <dd>{d.printer_mode === "none" ? "Configure later" : `${d.printer_mode} · ${d.printer_target}`}</dd>
                  </dl>
                )}
              </>
            ) : null}
            {error ? <Banner tone="danger">{error}</Banner> : null}
          </div>
        </div>
        <div className="wizard-foot">
          <Button onClick={() => (setError(null), setStep(Math.max(0, step - 1)))} disabled={step === 0 || busy}>
            Back
          </Button>
          <Button variant="primary" className="right" onClick={next} loading={busy}>
            {step === steps.length - 1 ? (d.path === "join" ? "Pair terminal" : "Finish Setup") : "Continue"}
          </Button>
        </div>
      </main>
    </div>
  );
}
