import { useEffect, useRef } from "react";
import { Percent, Tag, Trash2, Hash } from "lucide-react";
import type { Cart } from "../../api/types";
import { formatMoney, formatQty, formatPercent } from "../../lib/money";
import { Button } from "../../components/ui";

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
}) {
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!flashLine) return;
    listRef.current?.querySelector(`[data-line="${flashLine}"]`)?.scrollIntoView({ block: "nearest" });
  }, [flashLine, cart]);
  return (
    <div className="pos-panel">
      <div className="cart-head">
        <h3 className="grow">Current Sale</h3>
        <span className="tiny" data-testid="line-count">
          {cart.lines.length} {cart.lines.length === 1 ? "line" : "lines"} · {formatQty(cart.totals.item_count_milli)}{" "}
          items
        </span>
      </div>
      <div className="cart-lines" ref={listRef} role="list" aria-label="Cart">
        {cart.lines.length === 0 ? (
          <div className="empty">
            <h3>No items yet</h3>
            <p>Scan a barcode to start a sale.</p>
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
                  <button aria-label={`Decrease ${l.name}`} onClick={() => onQty(l.line_id, -1)}>
                    −
                  </button>
                  <span onDoubleClick={() => onEditQty(l.line_id)}>
                    {formatQty(l.qty_milli)}
                    {l.unit !== "pcs" ? ` ${l.unit}` : ""}
                  </span>
                  <button aria-label={`Increase ${l.name}`} onClick={() => onQty(l.line_id, 1)}>
                    +
                  </button>
                </span>
                <span className="num">× {formatMoney(l.unit_price_minor)}</span>
                {l.price_overridden ? <span className="chip warning">Price changed</span> : null}
                {l.discount_minor > 0 ? (
                  <span className="chip brand">
                    −{formatMoney(l.discount_minor)}
                    {l.line_discount_bp ? ` (${formatPercent(l.line_discount_bp)})` : ""}
                  </span>
                ) : null}
                <span className="grow" />
                <span className="ellipsis" style={{ maxWidth: 140 }}>
                  {l.barcode ?? l.sku ?? (l.is_custom ? "Custom item" : "")}
                </span>
              </div>
              {sel ? (
                <div className="line-actions" onClick={(e) => e.stopPropagation()}>
                  <Button size="sm" icon={<Hash size={14} />} onClick={() => onEditQty(l.line_id)}>
                    Qty
                  </Button>
                  <Button size="sm" icon={<Percent size={14} />} onClick={() => onDiscount(l.line_id)}>
                    Discount
                  </Button>
                  {canPriceOverride ? (
                    <Button size="sm" icon={<Tag size={14} />} onClick={() => onPrice(l.line_id)}>
                      Price
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="danger-outline"
                    icon={<Trash2 size={14} />}
                    onClick={() => onRemove(l.line_id)}
                  >
                    Remove
                  </Button>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}
