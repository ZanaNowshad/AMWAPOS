import { t } from "../../i18n";
export function methodLabel(m: string): string {
  switch (m) {
    case "cash":
      return t("Cash");
    case "card":
      return t("Card");
    case "benefitpay":
      return t("BenefitPay");
    case "wallet":
      return t("Wallet");
    case "bank_transfer":
      return t("Bank Transfer");
    default:
      return m;
  }
}
