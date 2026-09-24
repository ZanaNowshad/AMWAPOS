export function methodLabel(m: string): string {
  switch (m) {
    case "cash":
      return "Cash";
    case "card":
      return "Card";
    case "benefitpay":
      return "BenefitPay";
    case "bank_transfer":
      return "Bank Transfer";
    default:
      return m;
  }
}
