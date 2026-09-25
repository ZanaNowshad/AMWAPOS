import { useEffect, useRef } from "react";
import { Percent, Tag, Trash2, Hash, Star } from "lucide-react";
import type { Cart } from "../../api/types";
import { formatMoney, formatQty, formatPercent } from "../../lib/money";
import { Button } from "../../components/ui";
import { t } from "../../i18n";

export function CartPanel({
  cart,
  selectedLine,
  flashLine,
  onSelect,
  onQty,
  onRemove,
  onEditQty,
  onDiscount,
  onPrice,
  canPriceOverride,
  onRedeem,
}: {
  cart: Cart;
  selectedLine: string | null;
  flashLine: string | null;
  onSelect: (id: string) => void;
  onQty: (id: string, delta: number) => void;
  onRemove: (id: string) => void;
  onEditQty: (id: string) => void;
  onDiscount: (id: string) => void;
  onPrice: (id: string) => void;
  canPriceOverride: boolean;
  /** Loyalty: open the redeem dialog (only when loyalty is on and a customer is set). */
  onRedeem?: () => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!flashLine) return;
    listRef.current?.querySelector(`[data-line="${flashLine}"]`)?.scrollIntoView({ block: "nearest" });
  }, [flashLine, cart]);
  return (
    <div className="pos-panel">
      <div className="cart-head">
        <h3 className="grow">{t("Current Sale")}</h3>
        <span className="tiny" data-testid="line-count">
          {t(
            cart.lines.length === 1 ? "{0} line · {1} items" : "{0} lines · {1} items",
            cart.lines.length,
            formatQty(cart.totals.item_count_milli),
          )}
        </span>
      </div>
      <div className="cart-lines" ref={listRef} role="list" aria-label={t("Cart")}>
        {cart.lines.length === 0 ? (
          <div className="empty">
            <h3>{t("No items yet")}</h3>
            <p>{t("Scan a barcode to start a sale.")}</p>
          </div>
        ) : null}
        {cart.lines.map((l) => {
          const sel = l.line_id === selectedLine;
          return (
            <div
              key={l.line_id}
              data-line={l.line_id}
              role="listitem"
              className={`cart-line ${sel ? "sel" : ""} ${l.line_id === flashLine ? "flash" : ""}`}
              onClick={() => onSelect(l.line_id)}
            >
              <div className="l1">
                <span className="l-name ellipsis">{l.name}</span>
                <span className="l-total">{formatMoney(l.line_total_minor)}</span>
              </div>
              <div className="l2">
                <span className="qty-ctl" onClick={(e) => e.stopPropagation()}>
                  <button aria-label={t("Decrease {0}", l.name)} onClick={() => onQty(l.line_id, -1)}>
                    −
                  </button>
                  <span onDoubleClick={() => onEditQty(l.line_id)}>
                    {formatQty(l.qty_milli)}
                    {l.unit !== "pcs" ? ` ${l.unit}` : ""}
                  </span>
                  <button aria-label={t("Increase {0}", l.name)} onClick={() => onQty(l.line_id, 1)}>
                    +
                  </button>
                </span>
                <span className="num">× {formatMoney(l.unit_price_minor)}</span>
                {l.price_overridden ? <span className="chip warning">{t("Price changed")}</span> : null}
                {lowStock(l) !== null ? (
                  <span
                    className="chip warning"
                    data-testid="low-stock-hint"
                    title={t("At or below the reorder point")}
                  >
                    {t("Low stock: {0} left", formatQty(lowStock(l)!))}
                  </span>
                ) : null}
                {l.discount_minor > 0 ? (
                  <span className="chip brand">
                    −{formatMoney(l.discount_minor)}
                    {l.line_discount_bp ? ` (${formatPercent(l.line_discount_bp)})` : ""}
                  </span>
                ) : null}
                <span className="grow" />
                <span className="ellipsis" style={{ maxWidth: 140 }}>
                  {l.barcode ?? l.sku ?? (l.is_custom ? t("Custom item") : "")}
                </span>
              </div>
              {sel ? (
                <div className="line-actions" onClick={(e) => e.stopPropagation()}>
                  <Button size="sm" icon={<Hash size={14} />} onClick={() => onEditQty(l.line_id)}>
                    {t("Qty")}
                  </Button>
                  <Button size="sm" icon={<Percent size={14} />} onClick={() => onDiscount(l.line_id)}>
                    {t("Discount")}
                  </Button>
                  {canPriceOverride ? (
                    <Button size="sm" icon={<Tag size={14} />} onClick={() => onPrice(l.line_id)}>
                      {t("Price")}
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="danger-outline"
                    icon={<Trash2 size={14} />}
                    onClick={() => onRemove(l.line_id)}
                  >
                    {t("Remove")}
                  </Button>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
      {cart.loyalty ? (
        <div className="cart-head" data-testid="loyalty-row" style={{ borderTop: "1px solid var(--line)" }}>
          <Star size={15} />
          <span className="grow small">
            {t("{0} points", cart.loyalty.balance)}
            {cart.loyalty.points > 0
              ? ` · ${t("redeeming {0} (−{1})", cart.loyalty.points, formatMoney(cart.loyalty.discount_minor))}`
              : ""}
            {cart.loyalty.earn_estimate > 0 ? ` · ${t("earns about {0}", cart.loyalty.earn_estimate)}` : ""}
          </span>
          {onRedeem ? (
            <Button size="sm" onClick={onRedeem}>
              {cart.loyalty.points > 0 ? t("Change") : t("Redeem")}
            </Button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Remaining stock after this line when it is at or below the reorder point. */
function lowStock(l: Cart["lines"][number]): number | null {
  if (l.stock_milli === null || l.reorder_point_milli === null || l.reorder_point_milli === undefined) return null;
  if (l.reorder_point_milli <= 0) return null;
  const left = l.stock_milli - l.qty_milli;
  return left <= l.reorder_point_milli ? left : null;
}
