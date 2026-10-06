import { useEffect, useRef } from "react";
import { Minus, Percent, Plus, ScanBarcode, Tag, Trash2, Hash, Star, UserRound } from "lucide-react";
import type { Cart, SaleChannel } from "../../api/types";
import { formatMoney, formatQty, formatPercent } from "../../lib/money";
import { Button } from "../../components/ui";
import { ProductImage } from "../../components/ProductImage";
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
  onCustomer,
  onChannel,
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
  onCustomer?: () => void;
  /** Where the sale comes from (decides which prices apply). */
  onChannel?: (c: SaleChannel) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!flashLine) return;
    listRef.current?.querySelector(`[data-line="${flashLine}"]`)?.scrollIntoView({ block: "nearest" });
  }, [flashLine, cart]);
  return (
    <div className="pos-panel cart">
      <div className="cart-head">
        <h3>{t("Current sale")}</h3>
        {cart.customer ? (
          <button type="button" className="customer-pill" onClick={onCustomer}>
            <UserRound size={16} aria-hidden />
            <span className="ellipsis" dir="auto">
              {cart.customer.name}
            </span>
          </button>
        ) : null}
        <span className="grow" />
        {onChannel && !cart.order ? (
          <select
            className="input channel-select"
            aria-label={t("Sale channel")}
            data-testid="sale-channel"
            value={cart.channel ?? "pos"}
            onChange={(e) => onChannel(e.target.value as SaleChannel)}
          >
            <option value="pos">{t("Till")}</option>
            <option value="phone">{t("Phone order")}</option>
            <option value="whatsapp">{t("WhatsApp")}</option>
            <option value="web">{t("Web")}</option>
            <option value="other">{t("Other")}</option>
          </select>
        ) : null}
        <span className="line-count" data-testid="line-count">
          {t(
            cart.lines.length === 1 ? "{0} line · {1} items" : "{0} lines · {1} items",
            cart.lines.length,
            formatQty(cart.totals.item_count_milli),
          )}
        </span>
      </div>
      <div className="cart-lines" ref={listRef} role="list" aria-label={t("Cart")}>
        {cart.lines.length === 0 ? (
          <div className="cart-empty">
            <div className="ce-art" aria-hidden>
              <ScanBarcode size={40} />
            </div>
            <h3>{t("No items yet")}</h3>
            <p>{t("Scan a barcode, or type a product name in the search box.")}</p>
          </div>
        ) : null}
        {cart.lines.map((l) => {
          const sel = l.line_id === selectedLine;
          const low = lowStock(l);
          return (
            <div
              key={l.line_id}
              data-line={l.line_id}
              role="listitem"
              className={`cart-line ${sel ? "sel" : ""} ${l.line_id === flashLine ? "flash" : ""}`}
              onClick={() => onSelect(l.line_id)}
            >
              <div className="l-main">
                <ProductImage hash={l.image_hash} name={l.name} size="sm" />
                <div className="l-info">
                  <span className="l-name ellipsis" dir="auto">
                    {l.name}
                  </span>
                  <span className="l-meta">
                    <span className="ellipsis l-code">
                      {l.barcode ?? l.sku ?? (l.is_custom ? t("Custom item") : "")}
                    </span>
                    <span className="money">× {formatMoney(l.unit_price_minor)}</span>
                    {low !== null ? (
                      <span className="chip warning" data-testid="low-stock-hint">
                        {t("Low stock: {0} left", formatQty(low))}
                      </span>
                    ) : null}
                    {l.price_overridden ? <span className="chip warning">{t("Price changed")}</span> : null}
                    {l.using_retail ? (
                      <span className="chip" data-testid="using-retail">
                        {t("Using retail price")}
                      </span>
                    ) : null}
                    {l.scale ? (
                      <span className="chip brand" data-testid="scale-line">
                        {l.scale.kind === "weight" ? t("Scale label: weight") : t("Scale label: price")}
                      </span>
                    ) : null}
                    {l.discount_minor > 0 ? (
                      <span className="chip brand money">
                        −{formatMoney(l.discount_minor)}
                        {l.line_discount_bp ? ` (${formatPercent(l.line_discount_bp)})` : ""}
                      </span>
                    ) : null}
                  </span>
                </div>
                <span className="qty-ctl" onClick={(e) => e.stopPropagation()}>
                  <button type="button" aria-label={t("Decrease {0}", l.name)} onClick={() => onQty(l.line_id, -1)}>
                    <Minus size={20} aria-hidden />
                  </button>
                  <button
                    type="button"
                    className="qty-val num"
                    aria-label={t("Quantity for {0}", l.name)}
                    onClick={() => onEditQty(l.line_id)}
                  >
                    {formatQty(l.qty_milli)}
                    {l.unit !== "pcs" ? <small> {l.unit}</small> : null}
                  </button>
                  <button type="button" aria-label={t("Increase {0}", l.name)} onClick={() => onQty(l.line_id, 1)}>
                    <Plus size={20} aria-hidden />
                  </button>
                </span>
                <span className="l-total money">{formatMoney(l.line_total_minor)}</span>
                <button
                  type="button"
                  className="l-del"
                  aria-label={t("Remove {0}", l.name)}
                  onClick={(e) => (e.stopPropagation(), onRemove(l.line_id))}
                >
                  <Trash2 size={20} aria-hidden />
                </button>
              </div>
              {sel ? (
                <div className="line-actions" onClick={(e) => e.stopPropagation()}>
                  <Button icon={<Hash size={18} />} onClick={() => onEditQty(l.line_id)}>
                    {t("Qty")}
                  </Button>
                  <Button icon={<Percent size={18} />} onClick={() => onDiscount(l.line_id)}>
                    {t("Discount")}
                  </Button>
                  {canPriceOverride ? (
                    <Button icon={<Tag size={18} />} onClick={() => onPrice(l.line_id)}>
                      {t("Price")}
                    </Button>
                  ) : null}
                  <Button variant="danger-outline" icon={<Trash2 size={18} />} onClick={() => onRemove(l.line_id)}>
                    {t("Remove")}
                  </Button>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
      {cart.loyalty ? (
        <div className="cart-head loyalty" data-testid="loyalty-row">
          <Star size={18} aria-hidden />
          <span className="grow small">
            {t("{0} points", cart.loyalty.balance)}
            {cart.loyalty.points > 0
              ? ` · ${t("redeeming {0} (−{1})", cart.loyalty.points, formatMoney(cart.loyalty.discount_minor))}`
              : ""}
            {cart.loyalty.earn_estimate > 0 ? ` · ${t("earns about {0}", cart.loyalty.earn_estimate)}` : ""}
          </span>
          {onRedeem ? <Button onClick={onRedeem}>{cart.loyalty.points > 0 ? t("Change") : t("Redeem")}</Button> : null}
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
