import { useEffect, useId, useRef, type ReactNode, type ButtonHTMLAttributes, type InputHTMLAttributes } from "react";
import { createPortal } from "react-dom";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { formatMoney } from "../lib/money";

type BtnProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "danger" | "danger-outline" | "ghost" | "default";
  size?: "sm" | "md" | "lg" | "xl";
  block?: boolean;
  icon?: ReactNode;
  kbd?: string;
  loading?: boolean;
};

export function Button({
  variant = "default",
  size = "md",
  block,
  icon,
  kbd,
  loading,
  className = "",
  children,
  disabled,
  ...rest
}: BtnProps) {
  const cls = [
    "btn",
    variant !== "default" ? variant : "",
    size !== "md" ? size : "",
    block ? "block" : "",
    !children ? "icon" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button type="button" className={cls} disabled={disabled || loading} {...rest}>
      {loading ? <span className="spinner" aria-hidden /> : icon}
      {children}
      {kbd ? <span className="kbd">{kbd}</span> : null}
    </button>
  );
}

export function Field({
  label,
  hint,
  error,
  required,
  children,
  className = "",
  htmlFor,
}: {
  label?: string;
  hint?: ReactNode;
  error?: string | null;
  required?: boolean;
  children: ReactNode;
  className?: string;
  htmlFor?: string;
}) {
  return (
    <div className={`field ${className}`}>
      {label ? (
        <label htmlFor={htmlFor} className={required ? "req" : ""}>
          {label}
        </label>
      ) : null}
      {children}
      {error ? (
        <div className="error" role="alert">
          {error}
        </div>
      ) : hint ? (
        <div className="hint">{hint}</div>
      ) : null}
    </div>
  );
}

export function TextInput({
  label,
  hint,
  error,
  required,
  className = "",
  fieldClass = "",
  ...rest
}: InputHTMLAttributes<HTMLInputElement> & {
  label?: string;
  hint?: ReactNode;
  error?: string | null;
  fieldClass?: string;
}) {
  const id = useId();
  return (
    <Field label={label} hint={hint} error={error} required={required} htmlFor={rest.id ?? id} className={fieldClass}>
      <input
        id={rest.id ?? id}
        className={`input ${error ? "invalid" : ""} ${className}`}
        aria-invalid={!!error}
        {...rest}
      />
    </Field>
  );
}

export function Checkbox({
  label,
  checked,
  onChange,
  disabled,
}: {
  label: ReactNode;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className="checkbox">
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <span>{label}</span>
    </label>
  );
}

export function Money({ minor, className = "" }: { minor: number | null | undefined; className?: string }) {
  return (
    <span className={`money ${minor !== null && minor !== undefined && minor < 0 ? "neg-num" : ""} ${className}`}>
      {formatMoney(minor)}
    </span>
  );
}

export function Chip({
  tone = "default",
  children,
  dot,
}: {
  tone?: "default" | "success" | "warning" | "danger" | "info" | "brand";
  children: ReactNode;
  dot?: boolean;
}) {
  return (
    <span className={`chip ${tone}`}>
      {dot ? <span className="dot" aria-hidden /> : null}
      {children}
    </span>
  );
}

const bannerIcon = { info: Info, success: CheckCircle2, warning: AlertTriangle, danger: XCircle };

export function Banner({
  tone = "info",
  title,
  children,
  action,
}: {
  tone?: "info" | "success" | "warning" | "danger";
  title?: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
}) {
  const Icon = bannerIcon[tone];
  return (
    <div className={`banner ${tone}`} role={tone === "danger" || tone === "warning" ? "alert" : "status"}>
      <Icon size={18} className="banner-icon" aria-hidden />
      <div className="grow">
        {title ? <div style={{ fontWeight: 650 }}>{title}</div> : null}
        {children ? <div>{children}</div> : null}
      </div>
      {action}
    </div>
  );
}

export function Empty({ title, children, actions }: { title: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="empty">
      <h3>{title}</h3>
      {children ? <p style={{ margin: "0 0 16px" }}>{children}</p> : null}
      {actions ? (
        <div className="row" style={{ justifyContent: "center" }}>
          {actions}
        </div>
      ) : null}
    </div>
  );
}

export function Skeleton({ rows = 5 }: { rows?: number }) {
  return (
    <div className="col" style={{ padding: 16, gap: 14 }} aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="skeleton" style={{ width: `${70 + ((i * 17) % 30)}%` }} />
      ))}
    </div>
  );
}

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function Modal({
  title,
  onClose,
  children,
  footer,
  size = "md",
  closeOnBackdrop = false,
  initialFocus,
  labelledBy,
}: {
  title: ReactNode;
  onClose?: () => void;
  children: ReactNode;
  footer?: ReactNode;
  size?: "sm" | "md" | "lg" | "xl" | "full";
  closeOnBackdrop?: boolean;
  initialFocus?: string;
  labelledBy?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useEffect(() => {
    const previously = document.activeElement as HTMLElement | null;
    const el = ref.current;
    const target =
      (initialFocus && el?.querySelector<HTMLElement>(initialFocus)) ||
      el?.querySelector<HTMLElement>("[autofocus]") ||
      el?.querySelector<HTMLElement>(FOCUSABLE);
    target?.focus();
    return () => {
      previously?.focus?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape" && onClose) {
      e.stopPropagation();
      onClose();
    }
    if (e.key === "Tab" && ref.current) {
      const items = Array.from(ref.current.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        last.focus();
        e.preventDefault();
      } else if (!e.shiftKey && document.activeElement === last) {
        first.focus();
        e.preventDefault();
      }
    }
  };
  return createPortal(
    <div className="backdrop" onMouseDown={(e) => closeOnBackdrop && e.target === e.currentTarget && onClose?.()}>
      <div
        ref={ref}
        className={`modal ${size}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy ?? titleId}
        onKeyDown={onKey}
      >
        <div className="modal-head">
          <h2 id={titleId} className="grow" style={{ fontSize: 18 }}>
            {title}
          </h2>
          {onClose ? <Button variant="ghost" aria-label="Close" icon={<X size={18} />} onClick={onClose} /> : null}
        </div>
        <div className="modal-body">{children}</div>
        {footer ? <div className="modal-foot">{footer}</div> : null}
      </div>
    </div>,
    document.body,
  );
}

export function Keypad({ onKey, extra }: { onKey: (k: string) => void; extra?: string }) {
  const keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", extra ?? ".", "0", "⌫"];
  return (
    <div className="keypad">
      {keys.map((k) => (
        <button
          key={k}
          type="button"
          onClick={() => onKey(k === "⌫" ? "Backspace" : k)}
          aria-label={k === "⌫" ? "Delete digit" : k}
        >
          {k}
        </button>
      ))}
    </div>
  );
}

export function Tabs<T extends string>({
  tabs,
  value,
  onChange,
}: {
  tabs: { key: T; label: string }[];
  value: T;
  onChange: (t: T) => void;
}) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((t) => (
        <button
          key={t.key}
          role="tab"
          aria-selected={value === t.key}
          className={`tab ${value === t.key ? "active" : ""}`}
          onClick={() => onChange(t.key)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export function StockStatus({ status }: { status: string }) {
  switch (status) {
    case "in_stock":
      return <Chip tone="success">In Stock</Chip>;
    case "low_stock":
      return <Chip tone="warning">Low Stock</Chip>;
    case "out_of_stock":
      return <Chip tone="danger">Out of Stock</Chip>;
    case "negative":
      return <Chip tone="danger">Negative</Chip>;
    default:
      return <Chip>Not tracked</Chip>;
  }
}

export function PageHeader({
  title,
  subtitle,
  actions,
  crumbs,
}: {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
  crumbs?: string;
}) {
  return (
    <div className="page-header">
      <div className="grow">
        {crumbs ? <div className="tiny">{crumbs}</div> : null}
        <h1>{title}</h1>
        {subtitle ? (
          <div className="muted" style={{ marginTop: 4 }}>
            {subtitle}
          </div>
        ) : null}
      </div>
      {actions ? <div className="row">{actions}</div> : null}
    </div>
  );
}
