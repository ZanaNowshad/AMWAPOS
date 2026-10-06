// Bahrain address entry: Flat / Building / Road / Block in one row, then a
// landmark line; governorate and directions behind "More details". With no parts typed, the line is a plain free-text address
// (older addresses keep working). The block fills the area.
import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { AddressParts } from "../api/types";
import { t } from "../i18n";

export interface AddrValue {
  flat: string;
  building: string;
  road: string;
  block: string;
  /** Landmark when parts are typed; otherwise the whole free-text address. */
  address: string;
  area: string;
  /** capital | muharraq | northern | southern, or "" (optional). */
  governorate?: string;
  /** How to get there (optional). */
  directions?: string;
}

export const emptyAddr: AddrValue = {
  flat: "",
  building: "",
  road: "",
  block: "",
  address: "",
  area: "",
  governorate: "",
  directions: "",
};

export const GOVERNORATES = ["capital", "muharraq", "northern", "southern"] as const;

export function governorateLabel(g: string | null | undefined): string {
  switch (g) {
    case "capital":
      return t("Capital Governorate");
    case "muharraq":
      return t("Muharraq Governorate");
    case "northern":
      return t("Northern Governorate");
    case "southern":
      return t("Southern Governorate");
    default:
      return "";
  }
}

const structured = (v: Pick<AddrValue, "flat" | "building" | "road" | "block">) =>
  !!(v.flat.trim() || v.building.trim() || v.road.trim() || v.block.trim());

/** Start from a saved customer (or drop): parts when it has them, else its line. */
export function addrFrom(
  src: { address?: string | null; area?: string | null; address_parts?: AddressParts | null } | null | undefined,
): AddrValue {
  const p = src?.address_parts;
  const extra = { governorate: p?.governorate ?? "", directions: p?.directions ?? "" };
  if (p && (p.flat || p.building || p.road || p.block)) {
    return {
      flat: p.flat ?? "",
      building: p.building ?? "",
      road: p.road ?? "",
      block: p.block ?? "",
      address: p.landmark ?? "",
      area: src?.area ?? "",
      ...extra,
    };
  }
  return { ...emptyAddr, address: src?.address ?? "", area: src?.area ?? "", ...extra };
}

/** What the backend takes: parts (and landmark) when typed, else the line. */
export function addrPayload(v: AddrValue): {
  address: string | null;
  area: string | null;
  address_parts: AddressParts | null;
} {
  const o = (s: string | undefined) => (s ?? "").trim() || null;
  const extra = { governorate: o(v.governorate), directions: o(v.directions) };
  if (!structured(v)) {
    const keep = extra.governorate || extra.directions;
    return { address: o(v.address), area: o(v.area), address_parts: keep ? extra : null };
  }
  return {
    address: null,
    area: o(v.area),
    address_parts: {
      flat: o(v.flat),
      building: o(v.building),
      road: o(v.road),
      block: o(v.block),
      landmark: o(v.address),
      ...extra,
    },
  };
}

export function addrIsEmpty(v: AddrValue): boolean {
  return !structured(v) && !v.address.trim() && !v.area.trim();
}

/** Parts typed but no building: the backend refuses it, so say so first. */
export function addrProblem(v: AddrValue): string | null {
  if (structured(v) && !v.building.trim()) return t("Enter the building or house number.");
  if (v.block.trim() && !/^\d+$/.test(v.block.trim())) return t("The block is a number, for example 256.");
  return null;
}

/** One line exactly as the backend stores and prints it ("Flat 12, Bldg 1203, Road 4518, Block 245"). */
export function addrLine(v: AddrValue): string {
  if (!structured(v)) return v.address.trim();
  return [
    v.flat.trim() && `Flat ${v.flat.trim()}`,
    v.building.trim() && `Bldg ${v.building.trim()}`,
    v.road.trim() && `Road ${v.road.trim()}`,
    v.block.trim() && `Block ${v.block.trim()}`,
    v.address.trim(),
  ]
    .filter(Boolean)
    .join(", ");
}

export function AddressFields({
  value,
  onChange,
  idPrefix = "addr",
}: {
  value: AddrValue;
  onChange: (v: AddrValue) => void;
  idPrefix?: string;
}) {
  // The area this component filled from the block; a typed area is never replaced.
  const autoArea = useRef<string | null>(null);
  const latest = useRef(value);
  latest.current = value;
  useEffect(() => {
    const block = value.block.trim();
    if (!/^\d{3,4}$/.test(block)) return;
    const tv = setTimeout(() => {
      api.customers
        .blockArea(block)
        .then((area) => {
          const cur = latest.current;
          if (!area || cur.block.trim() !== block) return;
          if (cur.area.trim() && cur.area !== autoArea.current) return;
          autoArea.current = area;
          onChange({ ...cur, area });
        })
        .catch(() => undefined);
    }, 250);
    return () => clearTimeout(tv);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value.block]);
  const set = (k: keyof AddrValue) => (e: React.ChangeEvent<HTMLInputElement>) =>
    onChange({ ...value, [k]: e.target.value });
  const parts = structured(value);
  const cell = (k: "flat" | "building" | "road" | "block", label: string, required = false) => (
    <div className="field addr-cell">
      <label htmlFor={`${idPrefix}-${k}`}>
        {label}
        {required && parts ? " *" : ""}
      </label>
      <input
        id={`${idPrefix}-${k}`}
        className="input num"
        inputMode={k === "block" || k === "road" ? "numeric" : "text"}
        value={value[k]}
        onChange={set(k)}
        autoComplete="off"
        data-testid={`${idPrefix}-${k}`}
      />
    </div>
  );
  const problem = addrProblem(value);
  // Progressive disclosure: governorate and directions only when asked for
  // (or already filled in).
  const [more, setMore] = useState(() => !!(value.governorate || value.directions));
  return (
    <div className="addr-fields col gap-8">
      <div className="addr-row">
        {cell("flat", t("Flat"))}
        {cell("building", t("Building"), true)}
        {cell("road", t("Road"))}
        {cell("block", t("Block"))}
      </div>
      <div className="field">
        <label htmlFor={`${idPrefix}-line`}>{parts ? t("Landmark (optional)") : t("Address")}</label>
        <input
          id={`${idPrefix}-line`}
          className="input"
          value={value.address}
          onChange={set("address")}
          placeholder={parts ? t("Near the mosque, blue gate…") : t("Or type the address")}
          data-testid={idPrefix === "send" ? "send-address" : `${idPrefix}-line`}
        />
      </div>
      {more ? (
        <div className="addr-row">
          <div className="field">
            <label htmlFor={`${idPrefix}-gov`}>{t("Governorate (optional)")}</label>
            <select
              id={`${idPrefix}-gov`}
              className="input"
              value={value.governorate ?? ""}
              onChange={(e) => onChange({ ...value, governorate: e.target.value })}
            >
              <option value="">{t("Not set")}</option>
              {GOVERNORATES.map((g) => (
                <option key={g} value={g}>
                  {governorateLabel(g)}
                </option>
              ))}
            </select>
          </div>
          <div className="field grow">
            <label htmlFor={`${idPrefix}-dir`}>{t("Directions (optional)")}</label>
            <input
              id={`${idPrefix}-dir`}
              className="input"
              value={value.directions ?? ""}
              maxLength={300}
              onChange={(e) => onChange({ ...value, directions: e.target.value })}
              placeholder={t("Second gate, ring twice…")}
            />
          </div>
        </div>
      ) : (
        <button type="button" className="link addr-more" onClick={() => setMore(true)}>
          {t("More details: governorate, directions")}
        </button>
      )}
      {problem ? (
        <div className="hint danger-text">{problem}</div>
      ) : parts ? (
        <div className="hint" dir="auto" data-testid={`${idPrefix}-preview`}>
          {addrLine(value)}
        </div>
      ) : null}
    </div>
  );
}
